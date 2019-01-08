use futures::future::Future;
use futures::stream::Stream;
use futures::sync::oneshot::Sender;
use http::request::Parts;
use hyper::body::Chunk;
use hyper::service::service_fn;
use hyper::Server;
use hyper::{Body, Method, Request, Response};
use std::borrow::Borrow;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::thread::JoinHandle;
use tokio::runtime::current_thread::Runtime;
use url::form_urlencoded;

pub struct TestServer {
    thread: JoinHandle<usize>,
    addr: SocketAddr,
    shutdown_tx: Sender<()>,
}

impl TestServer {
    pub fn new(func: &'static (impl Fn(Parts, Chunk) -> Response<Body> + Sync)) -> Self {
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

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn shutdown(self) -> usize {
        self.shutdown_tx.send(()).expect("shutdown_tx send");
        self.thread.join().expect("thread join")
    }
}

pub fn post_request<I, K, V>(pairs: I) -> Request<Body>
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
