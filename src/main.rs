use hyper::rt::{self, Future};
use hyper::service::service_fn;
use hyper::Server;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use rhubarb::challenge;
use rhubarb::http::hello;
use rhubarb::storage;

fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn main() {
    let addr = ([127, 0, 0, 1], 3000).into();
    let storage = Arc::new(Mutex::new(storage::storages::HashMap::new()));

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
