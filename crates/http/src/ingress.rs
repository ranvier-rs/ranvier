//! # Ingress Module - Flat API Entry Point
//!
//! Implements Discussion 193: `Ranvier::http()` is an **Ingress Circuit Builder**, not a web server.
//!
//! ## API Surface (MVP)
//!
//! - `bind(addr)` — Execution unit
//! - `route(path, circuit)` — Core wiring
//! - `fallback(circuit)` — Circuit completeness
//! - `into_raw_service()` — Escape hatch to Raw API
//!
//! ## Flat API Principle (Discussion 192)
//!
//! User code depth ≤ 2. Complexity is isolated, not hidden.

use bytes::Bytes;
use http::{Method, Request, Response, StatusCode};
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use ranvier_core::prelude::*;
use std::collections::HashMap;
use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use tokio::net::TcpListener;
use tower::Service;

/// The Ranvier Framework entry point.
///
/// `Ranvier` provides static methods to create Ingress builders for various protocols.
/// Currently only HTTP is supported.
pub struct Ranvier;

impl Ranvier {
    /// Create an HTTP Ingress Circuit Builder.
    ///
    /// This is the primary entry point for building HTTP applications with Ranvier.
    /// The returned builder uses a flat API (depth ≤ 2) per Discussion 192.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// Ranvier::http()
    ///     .bind("127.0.0.1:3000")
    ///     .route("/", my_circuit)
    ///     .run()
    ///     .await?;
    /// ```
    pub fn http() -> HttpIngress {
        HttpIngress::new()
    }
}

/// Route handler type: boxed async function returning Response
type RouteHandler = Arc<
    dyn Fn(Request<Incoming>) -> Pin<Box<dyn Future<Output = Response<Full<Bytes>>> + Send>>
        + Send
        + Sync,
>;

/// HTTP Ingress Circuit Builder.
///
/// Wires HTTP inputs to Ranvier Circuits. This is NOT a web server—it's a circuit wiring tool.
///
/// **Ingress is part of Schematic** (separate layer: Ingress → Circuit → Egress)
pub struct HttpIngress {
    /// Bind address (e.g., "127.0.0.1:3000")
    addr: Option<String>,
    /// Routes: (Method, Path) -> Handler
    routes: HashMap<(Method, String), RouteHandler>,
    /// Fallback circuit for unmatched routes
    fallback: Option<RouteHandler>,
}

impl HttpIngress {
    /// Create a new empty HttpIngress builder.
    pub fn new() -> Self {
        Self {
            addr: None,
            routes: HashMap::new(),
            fallback: None,
        }
    }

    /// Set the bind address for the server.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// Ranvier::http().bind("127.0.0.1:3000")
    /// ```
    pub fn bind(mut self, addr: impl Into<String>) -> Self {
        self.addr = Some(addr.into());
        self
    }

    /// Register a route with a circuit.
    ///
    /// The circuit receives `()` as input and should produce a `String` output.
    /// For more complex input handling, use the Bus to pass request data.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let hello = Axon::new("Hello").then(GreetBlock);
    /// Ranvier::http()
    ///     .route("/", hello)
    ///     .route("/users", user_circuit)
    /// ```
    pub fn route<E>(mut self, path: impl Into<String>, circuit: Axon<(), String, E>) -> Self
    where
        E: Send + 'static + std::fmt::Debug,
    {
        let path_str: String = path.into();
        let circuit = Arc::new(circuit);

        let handler: RouteHandler = Arc::new(move |_req: Request<Incoming>| {
            let circuit = circuit.clone();
            Box::pin(async move {
                let mut bus = Bus::new();
                let result = circuit.execute((), &mut bus).await;

                match result {
                    Outcome::Next(body) => Response::builder()
                        .status(StatusCode::OK)
                        .body(Full::new(Bytes::from(body)))
                        .unwrap(),
                    Outcome::Fault(e) => Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .body(Full::new(Bytes::from(format!("Error: {:?}", e))))
                        .unwrap(),
                    _ => Response::builder()
                        .status(StatusCode::OK)
                        .body(Full::new(Bytes::from("OK")))
                        .unwrap(),
                }
            }) as Pin<Box<dyn Future<Output = Response<Full<Bytes>>> + Send>>
        });

        self.routes.insert((Method::GET, path_str), handler);
        self
    }

    /// Register a route with a specific HTTP method.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// Ranvier::http()
    ///     .route_method(Method::POST, "/users", create_user_circuit)
    /// ```
    pub fn route_method<E>(
        mut self,
        method: Method,
        path: impl Into<String>,
        circuit: Axon<(), String, E>,
    ) -> Self
    where
        E: Send + 'static + std::fmt::Debug,
    {
        let path_str: String = path.into();
        let circuit = Arc::new(circuit);

        let handler: RouteHandler = Arc::new(move |_req: Request<Incoming>| {
            let circuit = circuit.clone();
            Box::pin(async move {
                let mut bus = Bus::new();
                let result = circuit.execute((), &mut bus).await;

                match result {
                    Outcome::Next(body) => Response::builder()
                        .status(StatusCode::OK)
                        .body(Full::new(Bytes::from(body)))
                        .unwrap(),
                    Outcome::Fault(e) => Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .body(Full::new(Bytes::from(format!("Error: {:?}", e))))
                        .unwrap(),
                    _ => Response::builder()
                        .status(StatusCode::OK)
                        .body(Full::new(Bytes::from("OK")))
                        .unwrap(),
                }
            }) as Pin<Box<dyn Future<Output = Response<Full<Bytes>>> + Send>>
        });

        self.routes.insert((method, path_str), handler);
        self
    }

    /// Set a fallback circuit for unmatched routes.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let not_found = Axon::new("NotFound").then(|_| async { "404 Not Found" });
    /// Ranvier::http()
    ///     .route("/", home)
    ///     .fallback(not_found)
    /// ```
    pub fn fallback<E>(mut self, circuit: Axon<(), String, E>) -> Self
    where
        E: Send + 'static + std::fmt::Debug,
    {
        let circuit = Arc::new(circuit);

        let handler: RouteHandler = Arc::new(move |_req: Request<Incoming>| {
            let circuit = circuit.clone();
            Box::pin(async move {
                let mut bus = Bus::new();
                let result = circuit.execute((), &mut bus).await;

                match result {
                    Outcome::Next(body) => Response::builder()
                        .status(StatusCode::NOT_FOUND)
                        .body(Full::new(Bytes::from(body)))
                        .unwrap(),
                    _ => Response::builder()
                        .status(StatusCode::NOT_FOUND)
                        .body(Full::new(Bytes::from("Not Found")))
                        .unwrap(),
                }
            }) as Pin<Box<dyn Future<Output = Response<Full<Bytes>>> + Send>>
        });

        self.fallback = Some(handler);
        self
    }

    /// Run the HTTP server.
    ///
    /// This starts The Hyper server and listens for incoming connections.
    /// The server will run until interrupted.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// Ranvier::http()
    ///     .bind("127.0.0.1:3000")
    ///     .route("/", hello)
    ///     .run()
    ///     .await?;
    /// ```
    pub async fn run(self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let addr_str = self.addr.as_deref().unwrap_or("127.0.0.1:3000");
        let addr: SocketAddr = addr_str.parse()?;

        let routes = Arc::new(self.routes);
        let fallback = self.fallback;

        let listener = TcpListener::bind(addr).await?;
        tracing::info!("Ranvier HTTP Ingress listening on http://{}", addr);

        loop {
            let (stream, _) = listener.accept().await?;
            let io = TokioIo::new(stream);

            let routes = routes.clone();
            let fallback = fallback.clone();

            tokio::task::spawn(async move {
                let service = service_fn(move |req: Request<Incoming>| {
                    let routes = routes.clone();
                    let fallback = fallback.clone();

                    async move {
                        let method = req.method().clone();
                        let path = req.uri().path().to_string();

                        // Try to find a matching route
                        if let Some(handler) = routes.get(&(method.clone(), path.clone())) {
                            Ok::<_, Infallible>(handler(req).await)
                        } else if let Some(ref fb) = fallback {
                            Ok(fb(req).await)
                        } else {
                            // Default 404
                            Ok(Response::builder()
                                .status(StatusCode::NOT_FOUND)
                                .body(Full::new(Bytes::from("Not Found")))
                                .unwrap())
                        }
                    }
                });

                if let Err(err) = http1::Builder::new().serve_connection(io, service).await {
                    tracing::error!("Error serving connection: {:?}", err);
                }
            });
        }
    }

    /// Convert to a raw Tower Service for integration with existing Tower stacks.
    ///
    /// This is the "escape hatch" per Discussion 193:
    /// > "Raw API는 Flat API의 탈출구다."
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let ingress = Ranvier::http()
    ///     .bind(":3000")
    ///     .route("/", circuit);
    ///
    /// let raw_service = ingress.into_raw_service();
    /// // Use raw_service with existing Tower infrastructure
    /// ```
    pub fn into_raw_service(
        self,
    ) -> impl Service<
        Request<Incoming>,
        Response = Response<Full<Bytes>>,
        Error = Infallible,
        Future = Pin<Box<dyn Future<Output = Result<Response<Full<Bytes>>, Infallible>> + Send>>,
    > + Clone {
        let routes = Arc::new(self.routes);
        let fallback = self.fallback;

        RawIngressService { routes, fallback }
    }
}

impl Default for HttpIngress {
    fn default() -> Self {
        Self::new()
    }
}

/// Internal service type for `into_raw_service()`
#[derive(Clone)]
struct RawIngressService {
    routes: Arc<HashMap<(Method, String), RouteHandler>>,
    fallback: Option<RouteHandler>,
}

impl Service<Request<Incoming>> for RawIngressService {
    type Response = Response<Full<Bytes>>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request<Incoming>) -> Self::Future {
        let routes = self.routes.clone();
        let fallback = self.fallback.clone();

        Box::pin(async move {
            let method = req.method().clone();
            let path = req.uri().path().to_string();

            if let Some(handler) = routes.get(&(method, path)) {
                Ok(handler(req).await)
            } else if let Some(ref fb) = fallback {
                Ok(fb(req).await)
            } else {
                Ok(Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Full::new(Bytes::from("Not Found")))
                    .unwrap())
            }
        })
    }
}
