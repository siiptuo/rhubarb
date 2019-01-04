use futures::future;
use futures::stream::Stream;
use hmac::{Hmac, Mac};
use hyper::rt::{self, Future};
use hyper::service::service_fn;
use hyper::{header::HeaderValue, Body, Client, Method, Request, Response, Server, StatusCode};
use sha2::Sha512;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use url::{form_urlencoded, Url};

mod challenge {
    pub struct Challenge(String);

    impl Challenge {
        pub fn as_str(&self) -> &str {
            self.0.as_str()
        }

        pub fn as_bytes(&self) -> &[u8] {
            self.0.as_bytes()
        }
    }

    pub trait Generator {
        fn generate(self) -> Challenge;
    }

    pub mod generators {
        use super::*;
        use rand::{distributions::Alphanumeric, thread_rng, Rng};

        pub struct Random;

        impl Random {
            pub fn new() -> Self {
                Self {}
            }
        }

        impl Generator for Random {
            fn generate(self) -> Challenge {
                Challenge(thread_rng().sample_iter(&Alphanumeric).take(32).collect())
            }
        }

        #[cfg(test)]
        pub struct Static {
            challenge: String,
        }

        #[cfg(test)]
        impl Static {
            pub fn new(challenge: String) -> Self {
                Self { challenge }
            }
        }

        #[cfg(test)]
        impl Generator for Static {
            fn generate(self) -> Challenge {
                Challenge(self.challenge)
            }
        }
    }
}

#[derive(Debug)]
struct Item {
    callback: String,
    topic: String,
    expires: u64,
    secret: Option<String>,
}

trait Storage {
    fn get(&self, callback: String, topic: String) -> Option<Item>;
    fn list(&self, topic: &str) -> Vec<Item>;
    fn insert(&mut self, item: Item);
    fn remove(&mut self, item: Item);
}

struct HashMapStorage {
    hash_map: HashMap<(String, String), (u64, Option<String>)>,
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
            .map(|value| Item {
                callback,
                topic,
                expires: value.0,
                secret: value.1.clone(),
            })
    }

    fn list(&self, topic: &str) -> Vec<Item> {
        self.hash_map
            .iter()
            .filter(|(key, _val)| key.1 == topic)
            .map(|(key, val)| Item {
                callback: key.0.clone(),
                topic: key.1.clone(),
                expires: val.0,
                secret: val.1.clone(),
            })
            .collect()
    }

    fn insert(&mut self, item: Item) {
        println!("insert {:?}", item);
        self.hash_map
            .insert((item.callback, item.topic), (item.expires, item.secret));
    }

    fn remove(&mut self, item: Item) {
        println!("remove {:?}", item);
        self.hash_map.remove(&(item.callback, item.topic));
    }
}

fn homepage() -> Response<Body> {
    Response::builder().body(Body::from("Rhubarb")).unwrap()
}

fn bad_request(error: &'static str) -> Response<Body> {
    Response::builder()
        .status(StatusCode::BAD_REQUEST)
        .body(Body::from(error))
        .unwrap()
}

fn accepted() -> Response<Body> {
    Response::builder()
        .status(StatusCode::ACCEPTED)
        .body(Body::empty())
        .unwrap()
}

enum Mode {
    Subscribe,
    Unsubscribe,
}

fn hello(
    req: Request<Body>,
    challenge_generator: impl challenge::Generator + Send + 'static,
    storage: &Arc<Mutex<impl Storage + Send + 'static>>,
    timestamp: u64,
) -> Box<Future<Item = Response<Body>, Error = hyper::Error> + Send> {
    if req.method() != Method::POST {
        return Box::new(future::ok(homepage()));
    }
    let storage = storage.clone();
    let res = req.into_body().concat2().map(move |body| {
        let mut callback = None;
        let mut mode = None;
        let mut topic = None;
        let mut lease_seconds = 123;
        let mut secret = None;
        for (name, value) in form_urlencoded::parse(&body) {
            match name.as_ref() {
                "hub.callback" => match Url::parse(&value) {
                    Ok(url) => match url.scheme() {
                        "http" | "https" => callback = Some(value),
                        _ => return bad_request("hub.callback should be a HTTP or HTTPS URL"),
                    },
                    Err(_) => return bad_request("hub.callback should be a HTTP or HTTPS URL"),
                },
                "hub.mode" => match value.as_ref() {
                    "subscribe" => mode = Some(Mode::Subscribe),
                    "unsubscribe" => mode = Some(Mode::Unsubscribe),
                    _ => return bad_request("hub.mode should be subscribe or unsubscribe"),
                },
                "hub.topic" => match Url::parse(&value) {
                    Ok(url) => match url.scheme() {
                        "http" | "https" => topic = Some(value),
                        _ => return bad_request("hub.topic should be a HTTP or HTTPS URL"),
                    },
                    Err(_) => return bad_request("hub.topic should be a HTTP or HTTPS URL"),
                },
                "hub.lease_seconds" => match &value.parse::<u64>() {
                    Ok(val) => lease_seconds = *val,
                    Err(_) => return bad_request("hub.lease_seconds must be a positive integer"),
                },
                "hub.secret" => {
                    if value.len() < 200 {
                        secret = Some(value.to_string())
                    } else {
                        return bad_request("hub.secret must be less than 200 bytes in length");
                    }
                }
                _ => {}
            }
        }
        match (callback, mode, topic) {
            (None, _, _) => bad_request("hub.callback required"),
            (_, None, _) => bad_request("hub.mode required"),
            (_, _, None) => bad_request("hub.topic required"),
            (Some(callback), Some(mode), Some(topic)) => {
                // Verification of intent
                let challenge = challenge_generator.generate();
                let verification = Url::parse_with_params(
                    &callback,
                    &[
                        (
                            "hub.mode",
                            match mode {
                                Mode::Subscribe => "subscribe",
                                Mode::Unsubscribe => "unsubscribe",
                            },
                        ),
                        ("hub.topic", topic.as_ref()),
                        ("hub.challenge", challenge.as_str()),
                        ("hub.lease_seconds", &lease_seconds.to_string()),
                    ],
                )
                .unwrap();
                let callback = callback.to_string();
                let topic = topic.to_string();
                let req = Client::new()
                    .get(verification.into_string().parse().unwrap())
                    .and_then(move |res| {
                        res.into_body().concat2().map(move |body| {
                            if body.as_ref() == challenge.as_bytes() {
                                println!("callback ok: challenge accepted");
                                match mode {
                                    Mode::Subscribe => storage.lock().unwrap().insert(Item {
                                        callback,
                                        topic,
                                        expires: timestamp + lease_seconds,
                                        secret,
                                    }),
                                    Mode::Unsubscribe => storage.lock().unwrap().remove(Item {
                                        callback,
                                        topic,
                                        expires: timestamp + lease_seconds,
                                        secret,
                                    }),
                                }
                            } else {
                                println!("callback failed: invalid challenge");
                            }
                        })
                    })
                    .map_err(|err| eprintln!("callback failed: {}", err));
                rt::spawn(req);

                accepted()
            }
        }
    });
    Box::new(res)
}

fn content_distribution(
    storage: &Arc<Mutex<impl Storage + Send + 'static>>,
    timestamp: u64,
    topic: &str,
    content: &str,
) -> Box<Future<Item = (), Error = ()> + Send> {
    let subscribers: Vec<(String, Option<String>)> = storage
        .lock()
        .unwrap()
        .list(topic)
        .iter()
        .filter(|item| item.expires > timestamp)
        .map(|item| (item.callback.clone(), item.secret.clone()))
        .collect();
    let topic = topic.to_string();
    let content = content.to_string();
    Box::new(future::lazy(move || {
        for (callback, secret) in subscribers {
            let callback2 = callback.clone();
            let client = Client::new();
            let mut req = Request::new(Body::from(content.clone()));
            *req.method_mut() = Method::POST;
            *req.uri_mut() = callback.parse().unwrap();
            let headers = req.headers_mut();
            headers.insert(
                "Link",
                HeaderValue::from_str(&format!("<TODO>; rel=hub, <{}>; rel=self", topic)).unwrap(),
            );
            if let Some(secret) = secret {
                let mut mac = Hmac::<Sha512>::new_varkey(secret.as_bytes()).unwrap();
                mac.input(content.as_bytes());
                headers.insert(
                    "X-Hub-Signature",
                    HeaderValue::from_str(&format!("sha512={:x}", mac.result().code())).unwrap(),
                );
            }
            let post = client
                .request(req)
                .map(move |_res| println!("callback to {} successed", callback))
                .map_err(move |err| eprintln!("callback to {} failed: {}", callback2, err));
            rt::spawn(post);
        }
        Ok(())
    }))
}

fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn main() {
    let addr = ([127, 0, 0, 1], 3000).into();
    let storage = Arc::new(Mutex::new(HashMapStorage::new()));

    let service = move || {
        let storage = Arc::clone(&storage);
        service_fn(move |req| {
            hello(
                req,
                challenge::generators::Random::new(),
                &storage,
                current_timestamp(),
            )
        })
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

    #[test]
    fn release_secondsquest_homepage() {
        let timestamp = 1500000000;
        let storage = Arc::new(Mutex::new(HashMapStorage::new()));
        let req = Request::builder()
            .method("GET")
            .body(Body::empty())
            .unwrap();
        hello(
            req,
            challenge::generators::Static::new("test".to_string()),
            &storage,
            timestamp,
        )
        .and_then(|res| {
            assert_eq!(res.status(), StatusCode::OK);
            res.into_body().concat2().map(|body| {
                assert_eq!(std::str::from_utf8(&body), Ok("Rhubarb"));
            })
        })
        .poll()
        .unwrap();
    }

    #[test]
    fn hub_callback_required() {
        let timestamp = 1500000000;
        let storage = Arc::new(Mutex::new(HashMapStorage::new()));
        let req = Request::builder()
            .method("POST")
            .body(Body::from(""))
            .unwrap();
        hello(
            req,
            challenge::generators::Static::new("test".to_string()),
            &storage,
            timestamp,
        )
        .and_then(|res| {
            assert_eq!(res.status(), StatusCode::BAD_REQUEST);
            res.into_body().concat2().map(|body| {
                assert_eq!(std::str::from_utf8(&body), Ok("hub.callback required"));
            })
        })
        .poll()
        .unwrap();
    }

    #[test]
    fn hub_mode_required() {
        let timestamp = 1500000000;
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
            challenge::generators::Static::new("test".to_string()),
            &storage,
            timestamp,
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
        let timestamp = 1500000000;
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
            challenge::generators::Static::new("test".to_string()),
            &storage,
            timestamp,
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
        let timestamp = 1500000000;
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
            challenge::generators::Static::new("test".to_string()),
            &storage,
            timestamp,
        )
        .and_then(|res| {
            assert_eq!(res.status(), StatusCode::BAD_REQUEST);
            res.into_body().concat2().map(|body| {
                assert_eq!(std::str::from_utf8(&body), Ok("hub.topic required"));
            })
        })
        .poll()
        .unwrap();
    }

    #[test]
    fn hub_callback_not_http() {
        let timestamp = 1500000000;
        let storage = Arc::new(Mutex::new(HashMapStorage::new()));
        let req = Request::builder()
            .method("POST")
            .body(Body::from(
                form_urlencoded::Serializer::new(String::new())
                    .append_pair("hub.callback", "gopher://callback.local")
                    .append_pair("hub.mode", "subscribe")
                    .append_pair("hub.topic", "http://topic.local")
                    .finish(),
            ))
            .unwrap();
        hello(
            req,
            challenge::generators::Static::new("test".to_string()),
            &storage,
            timestamp,
        )
        .and_then(|res| {
            assert_eq!(res.status(), StatusCode::BAD_REQUEST);
            res.into_body().concat2().map(|body| {
                assert_eq!(
                    std::str::from_utf8(&body),
                    Ok("hub.callback should be a HTTP or HTTPS URL")
                );
            })
        })
        .poll()
        .unwrap();
    }

    #[test]
    fn hub_callback_not_url() {
        let timestamp = 1500000000;
        let storage = Arc::new(Mutex::new(HashMapStorage::new()));
        let req = Request::builder()
            .method("POST")
            .body(Body::from(
                form_urlencoded::Serializer::new(String::new())
                    .append_pair("hub.callback", "garbage")
                    .append_pair("hub.mode", "subscribe")
                    .append_pair("hub.topic", "http://topic.local")
                    .finish(),
            ))
            .unwrap();
        hello(
            req,
            challenge::generators::Static::new("test".to_string()),
            &storage,
            timestamp,
        )
        .and_then(|res| {
            assert_eq!(res.status(), StatusCode::BAD_REQUEST);
            res.into_body().concat2().map(|body| {
                assert_eq!(
                    std::str::from_utf8(&body),
                    Ok("hub.callback should be a HTTP or HTTPS URL")
                );
            })
        })
        .poll()
        .unwrap();
    }

    #[test]
    fn hub_topic_not_http() {
        let timestamp = 1500000000;
        let storage = Arc::new(Mutex::new(HashMapStorage::new()));
        let req = Request::builder()
            .method("POST")
            .body(Body::from(
                form_urlencoded::Serializer::new(String::new())
                    .append_pair("hub.callback", "http://callback.local")
                    .append_pair("hub.mode", "subscribe")
                    .append_pair("hub.topic", "gopher://topic.local")
                    .finish(),
            ))
            .unwrap();
        hello(
            req,
            challenge::generators::Static::new("test".to_string()),
            &storage,
            timestamp,
        )
        .and_then(|res| {
            assert_eq!(res.status(), StatusCode::BAD_REQUEST);
            res.into_body().concat2().map(|body| {
                assert_eq!(
                    std::str::from_utf8(&body),
                    Ok("hub.topic should be a HTTP or HTTPS URL")
                );
            })
        })
        .poll()
        .unwrap();
    }

    #[test]
    fn hub_topic_not_url() {
        let timestamp = 1500000000;
        let storage = Arc::new(Mutex::new(HashMapStorage::new()));
        let req = Request::builder()
            .method("POST")
            .body(Body::from(
                form_urlencoded::Serializer::new(String::new())
                    .append_pair("hub.callback", "http://callback.local")
                    .append_pair("hub.mode", "subscribe")
                    .append_pair("hub.topic", "garbage")
                    .finish(),
            ))
            .unwrap();
        hello(
            req,
            challenge::generators::Static::new("test".to_string()),
            &storage,
            timestamp,
        )
        .and_then(|res| {
            assert_eq!(res.status(), StatusCode::BAD_REQUEST);
            res.into_body().concat2().map(|body| {
                assert_eq!(
                    std::str::from_utf8(&body),
                    Ok("hub.topic should be a HTTP or HTTPS URL")
                );
            })
        })
        .poll()
        .unwrap();
    }

    #[test]
    fn hub_lease_seconds_not_integer() {
        let timestamp = 1500000000;
        let storage = Arc::new(Mutex::new(HashMapStorage::new()));
        let req = Request::builder()
            .method("POST")
            .body(Body::from(
                form_urlencoded::Serializer::new(String::new())
                    .append_pair("hub.callback", "http://callback.local")
                    .append_pair("hub.mode", "subscribe")
                    .append_pair("hub.topic", "http://topic.local")
                    .append_pair("hub.lease_seconds", "garbage")
                    .finish(),
            ))
            .unwrap();
        hello(
            req,
            challenge::generators::Static::new("test".to_string()),
            &storage,
            timestamp,
        )
        .and_then(|res| {
            assert_eq!(res.status(), StatusCode::BAD_REQUEST);
            res.into_body().concat2().map(|body| {
                assert_eq!(
                    std::str::from_utf8(&body),
                    Ok("hub.lease_seconds must be a positive integer")
                );
            })
        })
        .poll()
        .unwrap();
    }

    #[test]
    fn hub_lease_seconds_negative() {
        let timestamp = 1500000000;
        let storage = Arc::new(Mutex::new(HashMapStorage::new()));
        let req = Request::builder()
            .method("POST")
            .body(Body::from(
                form_urlencoded::Serializer::new(String::new())
                    .append_pair("hub.callback", "http://callback.local")
                    .append_pair("hub.mode", "subscribe")
                    .append_pair("hub.topic", "http://topic.local")
                    .append_pair("hub.lease_seconds", "-123")
                    .finish(),
            ))
            .unwrap();
        hello(
            req,
            challenge::generators::Static::new("test".to_string()),
            &storage,
            timestamp,
        )
        .and_then(|res| {
            assert_eq!(res.status(), StatusCode::BAD_REQUEST);
            res.into_body().concat2().map(|body| {
                assert_eq!(
                    std::str::from_utf8(&body),
                    Ok("hub.lease_seconds must be a positive integer")
                );
            })
        })
        .poll()
        .unwrap();
    }

    #[test]
    fn hub_too_long_secret() {
        let timestamp = 1500000000;
        let storage = Arc::new(Mutex::new(HashMapStorage::new()));
        let req = Request::builder()
            .method("POST")
            .body(Body::from(
                form_urlencoded::Serializer::new(String::new())
                    .append_pair("hub.callback", "http://callback.local")
                    .append_pair("hub.mode", "subscribe")
                    .append_pair("hub.topic", "http://topic.local")
                    .append_pair("hub.secret", &"!".repeat(200))
                    .finish(),
            ))
            .unwrap();
        hello(
            req,
            challenge::generators::Static::new("test".to_string()),
            &storage,
            timestamp,
        )
        .and_then(|res| {
            assert_eq!(res.status(), StatusCode::BAD_REQUEST);
            res.into_body().concat2().map(|body| {
                assert_eq!(
                    std::str::from_utf8(&body),
                    Ok("hub.secret must be less than 200 bytes in length")
                );
            })
        })
        .poll()
        .unwrap();
    }

    #[test]
    fn subscribe_minimal_success() {
        let addr = ([127, 0, 0, 1], 3004).into();
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

        let timestamp = 1500000000;
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
                challenge::generators::Static::new("test".to_string()),
                &storage,
                timestamp,
            )
            .map(|res| {
                assert_eq!(res.status(), StatusCode::ACCEPTED);
            })
            .map_err(|err| panic!(err)),
        );

        tx.send(()).unwrap();
        let requests = subscriber.join().unwrap();
        assert_eq!(requests, 1);

        let item = storage
            .lock()
            .unwrap()
            .get(callback, topic)
            .expect("Item not found");
        assert_eq!(item.expires, timestamp + 123);
        assert_eq!(item.secret, None);
    }

    #[test]
    fn subscribe_with_lease_seconds_success() {
        let addr = ([127, 0, 0, 1], 3006).into();
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
                        assert_eq!(params.get("hub.lease_seconds"), Some(&"321".to_string()));
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
        let timestamp = 1500000000;

        let req = Request::builder()
            .method("POST")
            .body(Body::from(
                form_urlencoded::Serializer::new(String::new())
                    .append_pair("hub.callback", &callback)
                    .append_pair("hub.mode", "subscribe")
                    .append_pair("hub.topic", &topic)
                    .append_pair("hub.lease_seconds", "321")
                    .finish(),
            ))
            .unwrap();

        rt::run(
            hello(
                req,
                challenge::generators::Static::new("test".to_string()),
                &storage,
                timestamp,
            )
            .map(|res| {
                assert_eq!(res.status(), StatusCode::ACCEPTED);
            })
            .map_err(|err| panic!(err)),
        );

        tx.send(()).unwrap();
        let requests = subscriber.join().unwrap();
        assert_eq!(requests, 1);

        let item = storage
            .lock()
            .unwrap()
            .get(callback, topic)
            .expect("Item not found");
        assert_eq!(item.expires, timestamp + 321);
        assert_eq!(item.secret, None);
    }

    #[test]
    fn subscribe_with_secret_success() {
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

        let timestamp = 1500000000;
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
                    .append_pair("hub.secret", "mysecret")
                    .finish(),
            ))
            .unwrap();

        rt::run(
            hello(
                req,
                challenge::generators::Static::new("test".to_string()),
                &storage,
                timestamp,
            )
            .map(|res| {
                assert_eq!(res.status(), StatusCode::ACCEPTED);
            })
            .map_err(|err| panic!(err)),
        );

        tx.send(()).unwrap();
        let requests = subscriber.join().unwrap();
        assert_eq!(requests, 1);

        let item = storage
            .lock()
            .unwrap()
            .get(callback, topic)
            .expect("Item not found");
        assert_eq!(item.expires, timestamp + 123);
        assert_eq!(item.secret, Some("mysecret".to_string()));
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

        let timestamp = 1500000000;
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
                challenge::generators::Static::new("test".to_string()),
                &storage,
                timestamp,
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

    #[test]
    fn unsubscribe_success() {
        let addr = ([127, 0, 0, 1], 3003).into();
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
                        assert_eq!(params.get("hub.mode"), Some(&"unsubscribe".to_string()));
                        assert_eq!(
                            params.get("hub.topic"),
                            Some(&"http://topic.local".to_string())
                        );
                        assert_eq!(params.get("hub.challenge"), Some(&"test".to_string()));
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

        let timestamp = 1500000000;

        let mut storage = HashMapStorage::new();
        storage.insert(Item {
            callback: format!("http://{}", addr),
            topic: "http://topic.local".to_string(),
            expires: timestamp + 123,
            secret: None,
        });
        let storage = Arc::new(Mutex::new(storage));

        let callback = format!("http://{}", addr);
        let topic = "http://topic.local".to_string();

        let req = Request::builder()
            .method("POST")
            .body(Body::from(
                form_urlencoded::Serializer::new(String::new())
                    .append_pair("hub.callback", &callback)
                    .append_pair("hub.mode", "unsubscribe")
                    .append_pair("hub.topic", &topic)
                    .finish(),
            ))
            .unwrap();

        rt::run(
            hello(
                req,
                challenge::generators::Static::new("test".to_string()),
                &storage,
                timestamp,
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

    #[test]
    fn content_distribution_expired() {
        let addr = ([127, 0, 0, 1], 3007).into();
        let (tx, rx) = futures::sync::oneshot::channel::<()>();
        let subscriber = thread::spawn(move || {
            let exec = runtime::current_thread::TaskExecutor::current();
            let counter = Rc::new(Cell::new(0));
            let counter2 = counter.clone();
            let server = Server::bind(&addr)
                .executor(exec)
                .serve(move || {
                    let cnt = counter2.clone();
                    service_fn_ok(move |_req| {
                        cnt.set(cnt.get() + 1);
                        Response::new(Body::empty())
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

        let timestamp = 1500000000;

        let mut storage = HashMapStorage::new();
        storage.insert(Item {
            callback: format!("http://{}", addr),
            topic: "http://topic.local".to_string(),
            expires: timestamp - 123,
            secret: None,
        });
        let storage = Arc::new(Mutex::new(storage));

        rt::run(
            content_distribution(&storage, timestamp, "http://topic.local", "breaking news")
                .map_err(|err| panic!(err)),
        );

        tx.send(()).unwrap();
        let requests = subscriber.join().unwrap();
        assert_eq!(requests, 0);
    }

    #[test]
    fn content_distribution_success() {
        let addr = ([127, 0, 0, 1], 3002).into();
        let (tx, rx) = futures::sync::oneshot::channel::<()>();
        let subscriber = thread::spawn(move || {
            let exec = runtime::current_thread::TaskExecutor::current();
            let counter = Rc::new(Cell::new(0));
            let counter2 = counter.clone();
            let server = Server::bind(&addr)
                .executor(exec)
                .serve(move || {
                    let cnt = counter2.clone();
                    service_fn(move |req: Request<Body>| {
                        cnt.set(cnt.get() + 1);

                        assert_eq!(req.method(), Method::POST);
                        assert_eq!(
                            req.headers().get("Link"),
                            Some(&HeaderValue::from_static(
                                "<TODO>; rel=hub, <http://topic.local>; rel=self"
                            ))
                        );
                        assert!(req.headers().get("X-Hub-Signature").is_none());

                        req.into_body().concat2().map(move |body| {
                            assert_eq!(std::str::from_utf8(&body), Ok("breaking news"));
                            Response::new(Body::empty())
                        })
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

        let timestamp = 1500000000;

        let mut storage = HashMapStorage::new();
        storage.insert(Item {
            callback: format!("http://{}", addr),
            topic: "http://topic.local".to_string(),
            expires: timestamp + 123,
            secret: None,
        });
        let storage = Arc::new(Mutex::new(storage));

        rt::run(
            content_distribution(&storage, timestamp, "http://topic.local", "breaking news")
                .map_err(|err| panic!(err)),
        );

        tx.send(()).unwrap();
        let requests = subscriber.join().unwrap();
        assert_eq!(requests, 1);
    }

    #[test]
    fn authenticated_content_distribution_success() {
        let addr = ([127, 0, 0, 1], 3005).into();
        let (tx, rx) = futures::sync::oneshot::channel::<()>();
        let subscriber = thread::spawn(move || {
            let exec = runtime::current_thread::TaskExecutor::current();
            let counter = Rc::new(Cell::new(0));
            let counter2 = counter.clone();
            let server = Server::bind(&addr)
                .executor(exec)
                .serve(move || {
                    let cnt = counter2.clone();
                    service_fn(move |req: Request<Body>| {
                        cnt.set(cnt.get() + 1);

                        assert_eq!(req.method(), Method::POST);
                        assert_eq!(
                            req.headers().get("Link"),
                            Some(&HeaderValue::from_static(
                                "<TODO>; rel=hub, <http://topic.local>; rel=self"
                            ))
                        );
                        assert_eq!(
                            req.headers().get("X-Hub-Signature"),
                            Some(&HeaderValue::from_static("sha512=0f18aaef5a69a9bce743a284ffd054cb24a9faa349931f338015d32d0e37c2c01c4a95afc4173f5cc57e4c161c528dd68e13f0f00e37036224feaf438b2fd49b"))
                        );

                        req.into_body().concat2().map(move |body| {
                            assert_eq!(std::str::from_utf8(&body), Ok("breaking news"));
                            Response::new(Body::empty())
                        })
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

        let timestamp = 1500000000;

        let mut storage = HashMapStorage::new();
        storage.insert(Item {
            callback: format!("http://{}", addr),
            topic: "http://topic.local".to_string(),
            expires: timestamp + 123,
            secret: Some("mysecret".to_string()),
        });
        let storage = Arc::new(Mutex::new(storage));

        rt::run(
            content_distribution(&storage, timestamp, "http://topic.local", "breaking news")
                .map_err(|err| panic!(err)),
        );

        tx.send(()).unwrap();
        let requests = subscriber.join().unwrap();
        assert_eq!(requests, 1);
    }
}
