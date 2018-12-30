use futures::future;
use futures::stream::Stream;
use hyper::rt::{self, Future};
use hyper::service::service_fn;
use hyper::{Body, Client, Method, Request, Response, Server, StatusCode};
use rand::distributions::Alphanumeric;
use rand::{thread_rng, Rng};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use url::{form_urlencoded, Url};

struct Challenge(String);

trait ChallengeGenerator {
    fn generate(self) -> Challenge;
}

struct RandomChallengeGenerator;

impl RandomChallengeGenerator {
    fn new() -> Self {
        Self {}
    }
}

impl ChallengeGenerator for RandomChallengeGenerator {
    fn generate(self) -> Challenge {
        Challenge(thread_rng().sample_iter(&Alphanumeric).take(32).collect())
    }
}

#[derive(Debug)]
struct Item {
    callback: String,
    topic: String,
    lease_seconds: u32,
}

trait Storage {
    fn get(&self, callback: String, topic: String) -> Option<Item>;
    fn insert(&mut self, item: Item);
    fn remove(&mut self, item: Item);
}

struct HashMapStorage {
    hash_map: HashMap<(String, String), u32>,
}

impl HashMapStorage {
    fn new() -> Self {
        Self {
            hash_map: HashMap::new(),
        }
    }
}

impl Storage for HashMapStorage {
    fn get(&self, callback: String, topic: String) -> Option<Item> {
        println!("get {{ callback: {}, topic: {} }}", callback, topic);
        self.hash_map
            .get(&(callback.clone(), topic.clone()))
            .map(|lease_seconds| Item {
                callback,
                topic,
                lease_seconds: *lease_seconds,
            })
    }

    fn insert(&mut self, item: Item) {
        println!("insert {:?}", item);
        self.hash_map
            .insert((item.callback, item.topic), item.lease_seconds);
    }

    fn remove(&mut self, item: Item) {
        println!("remove {:?}", item);
        self.hash_map.remove(&(item.callback, item.topic));
    }
}

fn hello(
    req: Request<Body>,
    challenge_generator: impl ChallengeGenerator + Send + 'static,
    storage: &Arc<Mutex<impl Storage + Send + 'static>>,
) -> Box<Future<Item = Response<Body>, Error = hyper::Error> + Send> {
    if req.method() != Method::POST {
        return Box::new(future::ok(
            Response::builder().body(Body::from("Rhubarb")).unwrap(),
        ));
    }
    let storage = storage.clone();
    let res = req.into_body().concat2().map(move |body| {
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
        match (callback, mode, topic) {
            (None, _, _) => Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from("hub.callback required"))
                .unwrap(),
            (_, None, _) => Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from("hub.mode required"))
                .unwrap(),
            (_, _, None) => Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from("hub.topic required"))
                .unwrap(),
            (Some(callback), Some(mode), Some(topic)) => {
                // Verification of intent
                let challenge = challenge_generator.generate();
                let verification = Url::parse_with_params(
                    &callback,
                    &[
                        ("hub.mode", mode.as_ref()),
                        ("hub.topic", topic.as_ref()),
                        ("hub.challenge", challenge.0.as_ref()),
                        ("hub.lease_seconds", "123"),
                    ],
                )
                .unwrap();
                let callback = callback.to_string();
                let topic = topic.to_string();
                let req = Client::new()
                    .get(verification.into_string().parse().unwrap())
                    .and_then(move |res| {
                        res.into_body().concat2().map(move |body| {
                            if body.into_bytes() == challenge.0.as_bytes() {
                                println!("callback ok: challenge accepted");
                                storage.lock().unwrap().insert(Item {
                                    callback,
                                    topic,
                                    lease_seconds: 123,
                                });
                            } else {
                                println!("callback failed: invalid challenge");
                            }
                        })
                    })
                    .map_err(|err| eprintln!("callback failed: {}", err));
                rt::spawn(req);

                Response::builder()
                    .status(StatusCode::ACCEPTED)
                    .body(Body::empty())
                    .unwrap()
            }
        }
    });
    Box::new(res)
}

fn main() {
    let addr = ([127, 0, 0, 1], 3000).into();
    let storage = Arc::new(Mutex::new(HashMapStorage::new()));

    let service = move || {
        let storage = Arc::clone(&storage);
        service_fn(move |req| hello(req, RandomChallengeGenerator::new(), &storage))
    };

    let server = Server::bind(&addr)
        .serve(service)
        .map_err(|e| eprintln!("server error: {}", e));

    println!("Listening on http://{}", addr);

    rt::run(server);
}

#[cfg(test)]
mod tests {
    use super::*;

    use hyper::rt::{Future, Stream};
    use hyper::service::service_fn_ok;
    use std::cell::Cell;
    use std::rc::Rc;
    use std::thread;
    use tokio::runtime;

    struct StaticChallengeGenerator {
        challenge: String,
    }

    impl StaticChallengeGenerator {
        fn new(challenge: String) -> Self {
            Self { challenge }
        }
    }

    impl ChallengeGenerator for StaticChallengeGenerator {
        fn generate(self) -> Challenge {
            Challenge(self.challenge)
        }
    }

    #[test]
    fn request_homepage() {
        let storage = Arc::new(Mutex::new(HashMapStorage::new()));
        let req = Request::builder()
            .method("GET")
            .body(Body::empty())
            .unwrap();
        hello(
            req,
            StaticChallengeGenerator::new("test".to_string()),
            &storage,
        )
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
        let storage = Arc::new(Mutex::new(HashMapStorage::new()));
        let req = Request::builder()
            .method("POST")
            .body(Body::from(""))
            .unwrap();
        hello(
            req,
            StaticChallengeGenerator::new("test".to_string()),
            &storage,
        )
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
        let storage = Arc::new(Mutex::new(HashMapStorage::new()));
        let req = Request::builder()
            .method("POST")
            .body(Body::from(
                form_urlencoded::Serializer::new(String::new())
                    .append_pair("hub.callback", "http://callback.local")
                    .finish(),
            ))
            .unwrap();
        hello(
            req,
            StaticChallengeGenerator::new("test".to_string()),
            &storage,
        )
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
        let storage = Arc::new(Mutex::new(HashMapStorage::new()));
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
        hello(
            req,
            StaticChallengeGenerator::new("test".to_string()),
            &storage,
        )
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
        let storage = Arc::new(Mutex::new(HashMapStorage::new()));
        let req = Request::builder()
            .method("POST")
            .body(Body::from(
                form_urlencoded::Serializer::new(String::new())
                    .append_pair("hub.callback", "http://callback.local")
                    .append_pair("hub.mode", "subscribe")
                    .finish(),
            ))
            .unwrap();
        hello(
            req,
            StaticChallengeGenerator::new("test".to_string()),
            &storage,
        )
        .and_then(|res| {
            assert_eq!(res.status(), StatusCode::BAD_REQUEST);
            res.into_body().concat2().map(|body| {
                assert_eq!(body.as_ref(), b"hub.topic required");
            })
        })
        .poll()
        .unwrap();
    }

    #[test]
    fn subscribe_success() {
        let addr = ([127, 0, 0, 1], 3000).into();
        let (tx, rx) = futures::sync::oneshot::channel::<()>();
        let subscriber = thread::spawn(move || {
            let exec = runtime::current_thread::TaskExecutor::current();
            let counter = Rc::new(Cell::new(0));
            let counter2 = counter.clone();
            let server = Server::bind(&addr)
                .executor(exec)
                .serve(move || {
                    let cnt = counter2.clone();
                    service_fn_ok(move |req| {
                        let query = req.uri().query();
                        assert!(query.is_some());
                        let params = form_urlencoded::parse(query.unwrap().as_bytes())
                            .into_owned()
                            .collect::<HashMap<String, String>>();
                        assert_eq!(params.get("hub.mode"), Some(&"subscribe".to_string()));
                        assert_eq!(
                            params.get("hub.topic"),
                            Some(&"http://topic.local".to_string())
                        );
                        assert_eq!(params.get("hub.challenge"), Some(&"test".to_string()));
                        assert_eq!(params.get("hub.lease_seconds"), Some(&"123".to_string()));
                        cnt.set(cnt.get() + 1);
                        Response::new(Body::from("test"))
                    })
                })
                .with_graceful_shutdown(rx)
                .map_err(|err| eprintln!("server error: {}", err));
            runtime::current_thread::Runtime::new()
                .expect("rt new")
                .spawn(server)
                .run()
                .expect("rt run");
            counter.get()
        });

        let storage = Arc::new(Mutex::new(HashMapStorage::new()));

        let callback = format!("http://{}", addr);
        let topic = "http://topic.local".to_string();

        let req = Request::builder()
            .method("POST")
            .body(Body::from(
                form_urlencoded::Serializer::new(String::new())
                    .append_pair("hub.callback", &callback)
                    .append_pair("hub.mode", "subscribe")
                    .append_pair("hub.topic", &topic)
                    .finish(),
            ))
            .unwrap();

        rt::run(
            hello(
                req,
                StaticChallengeGenerator::new("test".to_string()),
                &storage,
            )
            .map(|res| {
                assert_eq!(res.status(), StatusCode::ACCEPTED);
            })
            .map_err(|err| panic!(err)),
        );

        tx.send(()).unwrap();
        let requests = subscriber.join().unwrap();
        assert_eq!(requests, 1);

        assert!(storage.lock().unwrap().get(callback, topic).is_some());
    }

    #[test]
    fn subscribe_invalid_response() {
        let addr = ([127, 0, 0, 1], 3001).into();
        let (tx, rx) = futures::sync::oneshot::channel::<()>();
        let subscriber = thread::spawn(move || {
            let exec = runtime::current_thread::TaskExecutor::current();
            let counter = Rc::new(Cell::new(0));
            let counter2 = counter.clone();
            let server = Server::bind(&addr)
                .executor(exec)
                .serve(move || {
                    let cnt = counter2.clone();
                    service_fn_ok(move |req| {
                        let query = req.uri().query();
                        assert!(query.is_some());
                        let params = form_urlencoded::parse(query.unwrap().as_bytes())
                            .into_owned()
                            .collect::<HashMap<String, String>>();
                        assert_eq!(params.get("hub.mode"), Some(&"subscribe".to_string()));
                        assert_eq!(
                            params.get("hub.topic"),
                            Some(&"http://topic.local".to_string())
                        );
                        assert_eq!(params.get("hub.challenge"), Some(&"test".to_string()));
                        assert_eq!(params.get("hub.lease_seconds"), Some(&"123".to_string()));
                        cnt.set(cnt.get() + 1);
                        Response::new(Body::from("invalid"))
                    })
                })
                .with_graceful_shutdown(rx)
                .map_err(|err| eprintln!("server error: {}", err));
            runtime::current_thread::Runtime::new()
                .expect("rt new")
                .spawn(server)
                .run()
                .expect("rt run");
            counter.get()
        });

        let storage = Arc::new(Mutex::new(HashMapStorage::new()));

        let callback = format!("http://{}", addr);
        let topic = "http://topic.local".to_string();

        let req = Request::builder()
            .method("POST")
            .body(Body::from(
                form_urlencoded::Serializer::new(String::new())
                    .append_pair("hub.callback", &callback)
                    .append_pair("hub.mode", "subscribe")
                    .append_pair("hub.topic", &topic)
                    .finish(),
            ))
            .unwrap();

        rt::run(
            hello(
                req,
                StaticChallengeGenerator::new("test".to_string()),
                &storage,
            )
            .map(|res| {
                assert_eq!(res.status(), StatusCode::ACCEPTED);
            })
            .map_err(|err| panic!(err)),
        );

        tx.send(()).unwrap();
        let requests = subscriber.join().unwrap();
        assert_eq!(requests, 1);

        assert!(storage.lock().unwrap().get(callback, topic).is_none());
    }
}
