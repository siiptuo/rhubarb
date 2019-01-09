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

use futures::future::Future;
use futures::stream::Stream;
use hyper::{rt, Body, Method, Request, Response, StatusCode};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use url::form_urlencoded;

use rhubarb::challenge;
use rhubarb::http::hello;
use rhubarb::storage::{self, Storage};

mod common;

use crate::common::{post_request, TestServer};

#[test]
fn hub_callback_wrong_content_type() {
    let timestamp = 1500000000;
    let storage = Arc::new(Mutex::new(storage::storages::HashMap::new()));
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
    let storage = Arc::new(Mutex::new(storage::storages::HashMap::new()));
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
    let storage = Arc::new(Mutex::new(storage::storages::HashMap::new()));
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
    let storage = Arc::new(Mutex::new(storage::storages::HashMap::new()));
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
                Ok("hub.mode must be subscribe or unsubscribe")
            );
        })
    })
    .poll()
    .unwrap();
}

#[test]
fn hub_topic_required() {
    let timestamp = 1500000000;
    let storage = Arc::new(Mutex::new(storage::storages::HashMap::new()));
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
    let storage = Arc::new(Mutex::new(storage::storages::HashMap::new()));
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
                Ok("hub.callback must be an HTTP or HTTPS URL")
            );
        })
    })
    .poll()
    .unwrap();
}

#[test]
fn hub_callback_not_url() {
    let timestamp = 1500000000;
    let storage = Arc::new(Mutex::new(storage::storages::HashMap::new()));
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
                Ok("hub.callback must be an HTTP or HTTPS URL")
            );
        })
    })
    .poll()
    .unwrap();
}

#[test]
fn hub_topic_not_http() {
    let timestamp = 1500000000;
    let storage = Arc::new(Mutex::new(storage::storages::HashMap::new()));
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
                Ok("hub.topic must be an HTTP or HTTPS URL")
            );
        })
    })
    .poll()
    .unwrap();
}

#[test]
fn hub_topic_not_url() {
    let timestamp = 1500000000;
    let storage = Arc::new(Mutex::new(storage::storages::HashMap::new()));
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
                Ok("hub.topic must be an HTTP or HTTPS URL")
            );
        })
    })
    .poll()
    .unwrap();
}

#[test]
fn hub_lease_seconds_not_integer() {
    let timestamp = 1500000000;
    let storage = Arc::new(Mutex::new(storage::storages::HashMap::new()));
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
    let storage = Arc::new(Mutex::new(storage::storages::HashMap::new()));
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
    let storage = Arc::new(Mutex::new(storage::storages::HashMap::new()));
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
    let storage = Arc::new(Mutex::new(storage::storages::HashMap::new()));

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

    let storage = Arc::new(Mutex::new(storage::storages::HashMap::new()));

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
    let storage = Arc::new(Mutex::new(storage::storages::HashMap::new()));

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
    let storage = Arc::new(Mutex::new(storage::storages::HashMap::new()));

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
