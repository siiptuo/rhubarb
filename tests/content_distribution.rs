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
use hyper::rt;
use hyper::{header::HeaderValue, Body, Method, Response};
use std::sync::{Arc, Mutex};

use rhubarb::http::content_distribution;
use rhubarb::storage::{self, Item, Storage};

mod common;

use crate::common::TestServer;

#[test]
fn content_distribution_expired() {
    let server = TestServer::new(&|_parts, _body| Response::new(Body::empty()));

    let timestamp = 1500000000;

    let mut storage = storage::storages::HashMap::new();
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

    let mut storage = storage::storages::HashMap::new();
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

    let mut storage = storage::storages::HashMap::new();
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
