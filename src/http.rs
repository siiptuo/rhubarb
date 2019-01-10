// Copyright (C) 2019 Tuomas Siipola
//
// This file is part of Rhubarb.
//
// Rhubarb is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// Rhubarb program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with Rhubarb.  If not, see <https://www.gnu.org/licenses/>.

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
    Publish,
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
        let mut url = None;
        for (name, value) in form_urlencoded::parse(&body) {
            match name.as_ref() {
                "hub.callback" => match Url::parse(&value) {
                    Ok(url) => match url.scheme() {
                        "http" | "https" => callback = Some(value),
                        _ => return bad_request("hub.callback must be an HTTP or HTTPS URL"),
                    },
                    Err(_) => return bad_request("hub.callback must be an HTTP or HTTPS URL"),
                },
                "hub.mode" => match value.as_ref() {
                    "subscribe" => mode = Some(Mode::Subscribe),
                    "unsubscribe" => mode = Some(Mode::Unsubscribe),
                    "publish" => mode = Some(Mode::Publish),
                    _ => return bad_request("hub.mode must be subscribe, unsubscribe or publish"),
                },
                "hub.topic" => match Url::parse(&value) {
                    Ok(url) => match url.scheme() {
                        "http" | "https" => topic = Some(value),
                        _ => return bad_request("hub.topic must be an HTTP or HTTPS URL"),
                    },
                    Err(_) => return bad_request("hub.topic must be an HTTP or HTTPS URL"),
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
                "hub.url" => match Url::parse(&value) {
                    Ok(url2) => match url2.scheme() {
                        "http" | "https" => url = Some(value),
                        _ => return bad_request("hub.url must be an HTTP or HTTPS URL"),
                    },
                    Err(_) => return bad_request("hub.url must be an HTTP or HTTPS URL"),
                },
                _ => {}
            }
        }
        match mode {
            None => bad_request("hub.mode required"),
            Some(Mode::Publish) => match url {
                None => bad_request("at least one hub.url is required"),
                Some(url) => {
                    let url = url.to_string();
                    let callbacks = storage.lock().unwrap().list(&url);
                    if !callbacks.is_empty() {
                        let res = Client::new()
                            .get((*url).parse().unwrap())
                            .map_err(|err| eprintln!("fetch failed: {:?}", err))
                            .and_then(move |res| {
                                let (parts, body) = res.into_parts();
                                body.concat2()
                                    .map_err(|err| eprintln!("fetch body failed: {:?}", err))
                                    .and_then(move |body| {
                                        content_distribution(
                                            &storage,
                                            timestamp,
                                            &url,
                                            std::str::from_utf8(&body).unwrap(),
                                            parts
                                                .headers
                                                .get("Content-Type")
                                                .map(|val| val.to_str().unwrap())
                                                .unwrap(),
                                        )
                                    })
                            })
                            .map_err(|err| eprintln!("content distribution failed: {:?}", err));
                        rt::spawn(res);
                    }
                    accepted()
                }
            },
            Some(mode) => {
                match (callback, topic) {
                    (None, _) => bad_request("hub.callback required"),
                    (_, None) => bad_request("hub.topic required"),
                    (Some(callback), Some(topic)) => {
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
                                        Mode::Publish => unreachable!(),
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
                                            Mode::Subscribe => {
                                                storage.lock().unwrap().insert(Item {
                                                    callback,
                                                    topic,
                                                    expires: timestamp + lease_seconds,
                                                    secret,
                                                })
                                            }
                                            Mode::Unsubscribe => {
                                                storage.lock().unwrap().remove(Item {
                                                    callback,
                                                    topic,
                                                    expires: timestamp + lease_seconds,
                                                    secret,
                                                })
                                            }
                                            Mode::Publish => unreachable!(),
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
    content_type: &str,
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
    let content_type = content_type.to_string();
    Box::new(future::lazy(move || {
        for (callback, secret) in subscribers {
            let callback2 = callback.clone();
            let client = Client::new();
            let mut req = Request::post(&callback)
                .header("Link", format!("<TODO>; rel=hub, <{}>; rel=self", topic))
                .header("Content-Type", content_type.as_str())
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
