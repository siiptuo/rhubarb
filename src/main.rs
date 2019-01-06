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
    match req.headers().get("Content-Type").map(|v| v.as_bytes()) {
        Some(b"application/x-www-form-urlencoded") => {}
        _ => {
            return Box::new(future::ok(bad_request(
                "Content-Type header must be application/x-www-form-urlencoded",
            )))
        }
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

    use futures::sync::oneshot::Sender;
    use http::request::Parts;
    use hyper::body::Chunk;
    use hyper::rt::{Future, Stream};
    use std::borrow::Borrow;
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;
    use std::thread::JoinHandle;
    use tokio::runtime::current_thread::Runtime;

    struct TestServer {
        thread: JoinHandle<usize>,
        addr: SocketAddr,
        shutdown_tx: Sender<()>,
    }

    impl TestServer {
        fn new(func: &'static (impl Fn(Parts, Chunk) -> Response<Body> + Sync)) -> Self {
            let (addr_tx, addr_rx) = std::sync::mpsc::channel();
            let (shutdown_tx, shutdown_rx) = futures::sync::oneshot::channel::<()>();

            let thread = thread::spawn(move || {
                let counter = Arc::new(AtomicUsize::new(0));
                let counter2 = counter.clone();

                let server = Server::bind(&([127, 0, 0, 1], 0).into()).serve(move || {
                    let cnt = counter2.clone();
                    service_fn(move |req: Request<Body>| {
                        cnt.fetch_add(1, Ordering::Relaxed);
                        let (parts, body) = req.into_parts();
                        body.concat2().map(move |body| func(parts, body))
                    })
                });

                addr_tx.send(server.local_addr()).expect("addr_tx send");

                let fut = server.with_graceful_shutdown(shutdown_rx);

                Runtime::new()
                    .expect("rt new")
                    .block_on(fut)
                    .expect("rt block on");

                counter.load(Ordering::Relaxed)
            });

            Self {
                addr: addr_rx.recv().expect("server addr rx"),
                thread,
                shutdown_tx,
            }
        }

        fn addr(&self) -> SocketAddr {
            self.addr
        }

        fn shutdown(self) -> usize {
            self.shutdown_tx.send(()).expect("shutdown_tx send");
            self.thread.join().expect("thread join")
        }
    }

    fn post_request<I, K, V>(pairs: I) -> Request<Body>
    where
        I: IntoIterator,
        I::Item: Borrow<(K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        Request::builder()
            .method(Method::POST)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(Body::from(
                form_urlencoded::Serializer::new(String::new())
                    .extend_pairs(pairs)
                    .finish(),
            ))
            .unwrap()
    }

    #[test]
    fn release_secondsquest_homepage() {
        let timestamp = 1500000000;
        let storage = Arc::new(Mutex::new(HashMapStorage::new()));
        let req = Request::builder()
            .method(Method::GET)
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
    fn hub_callback_wrong_content_type() {
        let timestamp = 1500000000;
        let storage = Arc::new(Mutex::new(HashMapStorage::new()));
        let req = Request::builder()
            .method(Method::POST)
            .body(Body::empty())
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
                    Ok("Content-Type header must be application/x-www-form-urlencoded")
                );
            })
        })
        .poll()
        .unwrap();
    }

    #[test]
    fn hub_callback_required() {
        let timestamp = 1500000000;
        let storage = Arc::new(Mutex::new(HashMapStorage::new()));
        let req = post_request::<_, &str, &str>(&[]);
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
        let req = post_request(&[("hub.callback", "http://callback.local")]);
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
        let req = post_request(&[
            ("hub.callback", "http://callback.local"),
            ("hub.mode", "asd"),
            ("hub.topic", "http://topic.local"),
        ]);
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
        let req = post_request(&[
            ("hub.callback", "http://callback.local"),
            ("hub.mode", "subscribe"),
        ]);
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
        let req = post_request(&[
            ("hub.callback", "gopher://callback.local"),
            ("hub.mode", "subscribe"),
            ("hub.topic", "http://topic.local"),
        ]);
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
        let req = post_request(&[
            ("hub.callback", "garbage"),
            ("hub.mode", "subscribe"),
            ("hub.topic", "http://topic.local"),
        ]);
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
        let req = post_request(&[
            ("hub.callback", "http://callback.local"),
            ("hub.mode", "subscribe"),
            ("hub.topic", "gopher://topic.local"),
        ]);
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
        let req = post_request(&[
            ("hub.callback", "http://callback.local"),
            ("hub.mode", "subscribe"),
            ("hub.topic", "garbage"),
        ]);
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
        let req = post_request(&[
            ("hub.callback", "http://callback.local"),
            ("hub.mode", "subscribe"),
            ("hub.topic", "http://topic.local"),
            ("hub.lease_seconds", "garbage"),
        ]);
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
        let req = post_request(&[
            ("hub.callback", "http://callback.local"),
            ("hub.mode", "subscribe"),
            ("hub.topic", "http://topic.local"),
            ("hub.lease_seconds", "-123"),
        ]);
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
        let req = post_request(&[
            ("hub.callback", "http://callback.local"),
            ("hub.mode", "subscribe"),
            ("hub.topic", "http://topic.local"),
            ("hub.secret", &"!".repeat(200)),
        ]);
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
        let server = TestServer::new(&|parts, _body| {
            let query = parts.uri.query();
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

            Response::new(Body::from("test"))
        });

        let timestamp = 1500000000;
        let storage = Arc::new(Mutex::new(HashMapStorage::new()));

        let callback = format!("http://{}", server.addr());
        let topic = "http://topic.local".to_string();

        let req = post_request(&[
            ("hub.callback", callback.as_str()),
            ("hub.mode", "subscribe"),
            ("hub.topic", topic.as_str()),
        ]);

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

        let requests = server.shutdown();
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
        let server = TestServer::new(&|parts, _body| {
            let query = parts.uri.query();
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

            Response::new(Body::from("test"))
        });

        let storage = Arc::new(Mutex::new(HashMapStorage::new()));

        let callback = format!("http://{}", server.addr());
        let topic = "http://topic.local".to_string();
        let timestamp = 1500000000;

        let req = post_request(&[
            ("hub.callback", callback.as_str()),
            ("hub.mode", "subscribe"),
            ("hub.topic", topic.as_str()),
            ("hub.lease_seconds", "321"),
        ]);

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

        let requests = server.shutdown();
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
        let server = TestServer::new(&|parts, _body| {
            let query = parts.uri.query();
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

            Response::new(Body::from("test"))
        });

        let timestamp = 1500000000;
        let storage = Arc::new(Mutex::new(HashMapStorage::new()));

        let callback = format!("http://{}", server.addr());
        let topic = "http://topic.local".to_string();

        let req = post_request(&[
            ("hub.callback", callback.as_str()),
            ("hub.mode", "subscribe"),
            ("hub.topic", topic.as_str()),
            ("hub.secret", "mysecret"),
        ]);

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

        let requests = server.shutdown();
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
        let server = TestServer::new(&|parts, _body| {
            let query = parts.uri.query();
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

            Response::new(Body::from("invalid"))
        });

        let timestamp = 1500000000;
        let storage = Arc::new(Mutex::new(HashMapStorage::new()));

        let callback = format!("http://{}", server.addr());
        let topic = "http://topic.local".to_string();

        let req = post_request(&[
            ("hub.callback", callback.as_str()),
            ("hub.mode", "subscribe"),
            ("hub.topic", topic.as_str()),
        ]);

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

        let requests = server.shutdown();
        assert_eq!(requests, 1);

        assert!(storage.lock().unwrap().get(callback, topic).is_none());
    }

    #[test]
    fn unsubscribe_success() {
        let server = TestServer::new(&|parts, _body| {
            let query = parts.uri.query();
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

            Response::new(Body::from("test"))
        });

        let timestamp = 1500000000;

        let mut storage = HashMapStorage::new();
        storage.insert(Item {
            callback: format!("http://{}", server.addr()),
            topic: "http://topic.local".to_string(),
            expires: timestamp + 123,
            secret: None,
        });
        let storage = Arc::new(Mutex::new(storage));

        let callback = format!("http://{}", server.addr());
        let topic = "http://topic.local".to_string();

        let req = post_request(&[
            ("hub.callback", callback.as_str()),
            ("hub.mode", "unsubscribe"),
            ("hub.topic", topic.as_str()),
        ]);

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

        let requests = server.shutdown();
        assert_eq!(requests, 1);

        assert!(storage.lock().unwrap().get(callback, topic).is_none());
    }

    #[test]
    fn content_distribution_expired() {
        let server = TestServer::new(&|_parts, _body| Response::new(Body::empty()));

        let timestamp = 1500000000;

        let mut storage = HashMapStorage::new();
        storage.insert(Item {
            callback: format!("http://{}", server.addr()),
            topic: "http://topic.local".to_string(),
            expires: timestamp - 123,
            secret: None,
        });
        let storage = Arc::new(Mutex::new(storage));

        rt::run(
            content_distribution(&storage, timestamp, "http://topic.local", "breaking news")
                .map_err(|err| panic!(err)),
        );

        let requests = server.shutdown();
        assert_eq!(requests, 0);
    }

    #[test]
    fn content_distribution_success() {
        let server = TestServer::new(&|parts, body| {
            assert_eq!(parts.method, Method::POST);
            assert_eq!(
                parts.headers.get("Link"),
                Some(&HeaderValue::from_static(
                    "<TODO>; rel=hub, <http://topic.local>; rel=self"
                ))
            );
            assert!(parts.headers.get("X-Hub-Signature").is_none());

            assert_eq!(std::str::from_utf8(&body), Ok("breaking news"));

            Response::new(Body::empty())
        });

        let timestamp = 1500000000;

        let mut storage = HashMapStorage::new();
        storage.insert(Item {
            callback: format!("http://{}", server.addr()),
            topic: "http://topic.local".to_string(),
            expires: timestamp + 123,
            secret: None,
        });
        let storage = Arc::new(Mutex::new(storage));

        rt::run(
            content_distribution(&storage, timestamp, "http://topic.local", "breaking news")
                .map_err(|err| panic!(err)),
        );

        let requests = server.shutdown();
        assert_eq!(requests, 1);
    }

    #[test]
    fn authenticated_content_distribution_success() {
        let server = TestServer::new(&|parts, body| {
            assert_eq!(parts.method, Method::POST);
            assert_eq!(
                parts.headers.get("Link"),
                Some(&HeaderValue::from_static(
                    "<TODO>; rel=hub, <http://topic.local>; rel=self"
                ))
            );
            assert_eq!(
                parts.headers.get("X-Hub-Signature"),
                Some(&HeaderValue::from_static("sha512=0f18aaef5a69a9bce743a284ffd054cb24a9faa349931f338015d32d0e37c2c01c4a95afc4173f5cc57e4c161c528dd68e13f0f00e37036224feaf438b2fd49b"))
            );

            assert_eq!(std::str::from_utf8(&body), Ok("breaking news"));

            Response::new(Body::empty())
        });

        let timestamp = 1500000000;

        let mut storage = HashMapStorage::new();
        storage.insert(Item {
            callback: format!("http://{}", server.addr()),
            topic: "http://topic.local".to_string(),
            expires: timestamp + 123,
            secret: Some("mysecret".to_string()),
        });
        let storage = Arc::new(Mutex::new(storage));

        rt::run(
            content_distribution(&storage, timestamp, "http://topic.local", "breaking news")
                .map_err(|err| panic!(err)),
        );

        let requests = server.shutdown();
        assert_eq!(requests, 1);
    }
}
