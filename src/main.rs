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
