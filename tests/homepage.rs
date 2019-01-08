use futures::future::Future;
use futures::stream::Stream;
use hyper::{Body, Method, Request, StatusCode};
use std::sync::{Arc, Mutex};

use rhubarb::{challenge, http::hello, storage};

#[test]
fn request_homepage() {
    let timestamp = 1500000000;
    let storage = Arc::new(Mutex::new(storage::storages::HashMap::new()));
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
