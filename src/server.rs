//! Ranvier Server module
//!
//! Provides a simplified API to serve Ranvier pipelines over HTTP.

use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tower::{Service, ServiceBuilder};

/// Serve an HTTP server on the given port.
///
/// This is the simplest way to start a Ranvier server.
///
/// # Example
/// ```ignore
/// use ranvier::prelude::*;
///
/// #[tokio::main]
/// async fn main() {
///     ranvier::serve(3000, |_req| async {
///         Response::new(Full::new(Bytes::from("Hello, Ranvier!")))
///     }).await.unwrap();
/// }
/// ```
pub async fn serve<F, Fut>(
    port: u16,
    handler: F,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    F: Fn(Request<Incoming>) -> Fut + Clone + Send + 'static,
    Fut: Future<Output = Result<Response<Full<Bytes>>, Infallible>> + Send,
{
    let addr: SocketAddr = format!("0.0.0.0:{}", port).parse()?;
    let listener = TcpListener::bind(addr).await?;

    println!("🚀 Server running on http://localhost:{}/", port);

    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let handler = handler.clone();

        tokio::task::spawn(async move {
            let svc = ServiceBuilder::new().service_fn(handler);
            let svc = TowerToHyperService::new(svc);

            if let Err(err) = http1::Builder::new().serve_connection(io, svc).await {
                eprintln!("Error serving connection: {:?}", err);
            }
        });
    }
}
