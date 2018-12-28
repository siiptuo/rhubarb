use futures::future;
use futures::stream::Stream;
use hyper::rt::{self, Future};
use hyper::service::service_fn;
use hyper::{Body, Method, Request, Response, Server, StatusCode};
use url::form_urlencoded;

fn hello(req: Request<Body>) -> Box<Future<Item = Response<Body>, Error = hyper::Error> + Send> {
    if req.method() != Method::POST {
        return Box::new(future::ok(
            Response::builder().body(Body::from("Rhubarb")).unwrap(),
        ));
    }
    let res = req.into_body().concat2().map(|body| {
        let mut callback = None;
        let mut mode = None;
        let mut topic = None;
        for (name, value) in form_urlencoded::parse(&body) {
            match name.as_ref() {
                "hub.callback" => callback = Some(value),
                "hub.mode" => match value.as_ref() {
                    "subscribe" | "unsubscribe" => {
                        mode = Some(value);
                    }
                    _ => {
                        return Response::builder()
                            .status(StatusCode::BAD_REQUEST)
                            .body(Body::from("hub.mode should be subscribe or unsubscribe"))
                            .unwrap();
                    }
                },
                "hub.topic" => topic = Some(value),
                _ => {}
            }
        }
        if callback.is_none() {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from("hub.callback required"))
                .unwrap();
        }
        if mode.is_none() {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from("hub.mode required"))
                .unwrap();
        }
        if topic.is_none() {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from("hub.topic required"))
                .unwrap();
        }

        Response::builder()
            .status(StatusCode::ACCEPTED)
            .body(Body::empty())
            .unwrap()
    });
    Box::new(res)
}

fn main() {
    let addr = ([127, 0, 0, 1], 3000).into();

    let server = Server::bind(&addr)
        .serve(|| service_fn(hello))
        .map_err(|e| eprintln!("server error: {}", e));

    println!("Listening on http://{}", addr);

    rt::run(server);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_homepage() {
        let req = Request::builder()
            .method("GET")
            .body(Body::empty())
            .unwrap();
        hello(req)
            .and_then(|res| {
                assert_eq!(res.status(), StatusCode::OK);
                res.into_body().concat2().map(|body| {
                    assert_eq!(body.as_ref(), b"Rhubarb");
                })
            })
            .poll()
            .unwrap();
    }

    #[test]
    fn hub_callback_required() {
        let req = Request::builder()
            .method("POST")
            .body(Body::from(""))
            .unwrap();
        hello(req)
            .and_then(|res| {
                assert_eq!(res.status(), StatusCode::BAD_REQUEST);
                res.into_body().concat2().map(|body| {
                    assert_eq!(body.as_ref(), b"hub.callback required");
                })
            })
            .poll()
            .unwrap();
    }

    #[test]
    fn hub_mode_required() {
        let req = Request::builder()
            .method("POST")
            .body(Body::from(
                form_urlencoded::Serializer::new(String::new())
                    .append_pair("hub.callback", "http://callback.local")
                    .finish(),
            ))
            .unwrap();
        hello(req)
            .and_then(|res| {
                assert_eq!(res.status(), StatusCode::BAD_REQUEST);
                res.into_body().concat2().map(|body| {
                    assert_eq!(std::str::from_utf8(&body), Ok("hub.mode required"));
                })
            })
            .poll()
            .unwrap();
    }

    #[test]
    fn hub_mode_invalid() {
        let req = Request::builder()
            .method("POST")
            .body(Body::from(
                form_urlencoded::Serializer::new(String::new())
                    .append_pair("hub.callback", "http://callback.local")
                    .append_pair("hub.mode", "asd")
                    .append_pair("hub.topic", "http://topic.local")
                    .finish(),
            ))
            .unwrap();
        hello(req)
            .and_then(|res| {
                assert_eq!(res.status(), StatusCode::BAD_REQUEST);
                res.into_body().concat2().map(|body| {
                    assert_eq!(
                        std::str::from_utf8(&body),
                        Ok("hub.mode should be subscribe or unsubscribe")
                    );
                })
            })
            .poll()
            .unwrap();
    }

    #[test]
    fn hub_topic_required() {
        let req = Request::builder()
            .method("POST")
            .body(Body::from(
                form_urlencoded::Serializer::new(String::new())
                    .append_pair("hub.callback", "http://callback.local")
                    .append_pair("hub.mode", "subscribe")
                    .finish(),
            ))
            .unwrap();
        hello(req)
            .and_then(|res| {
                assert_eq!(res.status(), StatusCode::BAD_REQUEST);
                res.into_body().concat2().map(|body| {
                    assert_eq!(body.as_ref(), b"hub.topic required");
                })
            })
            .poll()
            .unwrap();
    }
}
