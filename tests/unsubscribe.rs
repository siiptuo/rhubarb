use futures::future::Future;
use hyper::rt;
use hyper::{Body, Response, StatusCode};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use url::form_urlencoded;

use rhubarb::challenge;
use rhubarb::http::hello;
use rhubarb::storage::{self, Item, Storage};

mod common;

use crate::common::{post_request, TestServer};

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

    let mut storage = storage::storages::HashMap::new();
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
