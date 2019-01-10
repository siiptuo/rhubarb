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
use hyper::{header::HeaderValue, rt, Body, Method, Response, StatusCode};
use std::sync::{Arc, Mutex};

use rhubarb::challenge;
use rhubarb::http::hello;
use rhubarb::storage::{self, Item, Storage};

mod common;

use crate::common::{post_request, TestServer};

#[test]
fn missing_url() {
    let timestamp = 1500000000;
    let storage = Arc::new(Mutex::new(storage::storages::HashMap::new()));
    let req = post_request(&[("hub.mode", "publish")]);
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
                Ok("at least one hub.url is required")
            );
        })
    })
    .poll()
    .unwrap();
}

#[test]
fn invalid_url() {
    let timestamp = 1500000000;
    let storage = Arc::new(Mutex::new(storage::storages::HashMap::new()));
    let req = post_request(&[("hub.mode", "publish"), ("hub.url", "garbage")]);
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
                Ok("hub.url must be an HTTP or HTTPS URL")
            );
        })
    })
    .poll()
    .unwrap();
}

#[test]
fn duplicate_urls() {
    let timestamp = 1500000000;
    let storage = Arc::new(Mutex::new(storage::storages::HashMap::new()));
    let req = post_request(&[
        ("hub.mode", "publish"),
        ("hub.url", "http://topic.local"),
        ("hub.url", "http://topic.local"),
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
                Ok("duplicate values of hub.url are not allowed")
            );
        })
    })
    .poll()
    .unwrap();
}

#[test]
fn no_subscribers() {
    let topic = TestServer::new(&|parts, _body| {
        assert_eq!(parts.method, Method::GET);
        Response::new(Body::from("test"))
    });

    let timestamp = 1500000000;
    let storage = Arc::new(Mutex::new(storage::storages::HashMap::new()));

    let url = format!("http://{}", topic.addr());

    let req = post_request(&[("hub.mode", "publish"), ("hub.url", &url)]);

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

    assert_eq!(topic.shutdown(), 0);
}

#[test]
fn single_url_success() {
    let topic = TestServer::new(&|parts, _body| {
        assert_eq!(parts.method, Method::GET);
        Response::builder()
            .header("Content-Type", "text/plain")
            .body(Body::from("breaking news"))
            .unwrap()
    });

    let subscriber = TestServer::new(&|parts, body| {
        assert_eq!(parts.method, Method::POST);
        assert_eq!(
            parts.headers.get("Content-Type"),
            Some(&HeaderValue::from_static("text/plain"))
        );
        assert_eq!(std::str::from_utf8(&body), Ok("breaking news"));
        Response::new(Body::empty())
    });

    let topic_url = format!("http://{}", topic.addr());
    let callback = format!("http://{}/callback", subscriber.addr());

    let timestamp = 1500000000;

    let mut storage = storage::storages::HashMap::new();
    storage.insert(Item {
        callback: callback,
        topic: topic_url.clone(),
        expires: timestamp + 123,
        secret: None,
    });
    let storage = Arc::new(Mutex::new(storage));

    let req = post_request(&[("hub.mode", "publish"), ("hub.url", &topic_url)]);

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

    assert_eq!(topic.shutdown(), 1);
    assert_eq!(subscriber.shutdown(), 1);
}

#[test]
fn multiple_urls_success() {
    let topic1 = TestServer::new(&|parts, _body| {
        assert_eq!(parts.method, Method::GET);
        Response::builder()
            .header("Content-Type", "text/plain")
            .body(Body::from("breaking news"))
            .unwrap()
    });

    let topic2 = TestServer::new(&|parts, _body| {
        assert_eq!(parts.method, Method::GET);
        Response::builder()
            .header("Content-Type", "text/plain")
            .body(Body::from("another breaking news"))
            .unwrap()
    });

    let subscriber = TestServer::new(&|parts, _body| {
        assert_eq!(parts.method, Method::POST);
        assert_eq!(
            parts.headers.get("Content-Type"),
            Some(&HeaderValue::from_static("text/plain"))
        );
        Response::new(Body::empty())
    });

    let topic1_url = format!("http://{}", topic1.addr());
    let topic2_url = format!("http://{}", topic2.addr());
    let callback = format!("http://{}/callback", subscriber.addr());

    let timestamp = 1500000000;

    let mut storage = storage::storages::HashMap::new();
    storage.insert(Item {
        callback: callback.clone(),
        topic: topic1_url.clone(),
        expires: timestamp + 123,
        secret: None,
    });
    storage.insert(Item {
        callback: callback.clone(),
        topic: topic2_url.clone(),
        expires: timestamp + 123,
        secret: None,
    });
    let storage = Arc::new(Mutex::new(storage));

    let req = post_request(&[
        ("hub.mode", "publish"),
        ("hub.url", &topic1_url),
        ("hub.url", &topic2_url),
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

    assert_eq!(topic1.shutdown(), 1);
    assert_eq!(topic2.shutdown(), 1);
    assert_eq!(subscriber.shutdown(), 2);
}
