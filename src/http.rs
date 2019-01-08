use futures::future;
use futures::stream::Stream;
use hmac::{Hmac, Mac};
use hyper::rt::{self, Future};
use hyper::{header::HeaderValue, Body, Client, Method, Request, Response, StatusCode};
use sha2::Sha512;
use std::sync::{Arc, Mutex};
use url::{form_urlencoded, Url};

use crate::challenge;
use crate::storage::{Item, Storage};

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

pub fn hello(
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

pub fn content_distribution(
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
            let mut req = Request::post(&callback)
                .header("Link", format!("<TODO>; rel=hub, <{}>; rel=self", topic))
                .body(Body::from(content.clone()))
                .unwrap();
            if let Some(secret) = secret {
                let mut mac = Hmac::<Sha512>::new_varkey(secret.as_bytes()).unwrap();
                mac.input(content.as_bytes());
                req.headers_mut().insert(
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
