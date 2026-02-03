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
use ranvier_runtime::Axon;
use std::collections::HashMap;
use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use tokio::net::TcpListener;
use tower::Service;
use tracing::Instrument;

/// The Ranvier Framework entry point.
///
/// `Ranvier` provides static methods to create Ingress builders for various protocols.
/// Currently only HTTP is supported.
pub struct Ranvier;

impl Ranvier {
    /// Create an HTTP Ingress Circuit Builder.
    pub fn http<R>() -> HttpIngress<R>
    where
        R: ranvier_core::transition::ResourceRequirement + Clone,
    {
        HttpIngress::new()
    }
}

/// Route handler type: boxed async function returning Response
type RouteHandler<R> = Arc<
    dyn Fn(Request<Incoming>, &R) -> Pin<Box<dyn Future<Output = Response<Full<Bytes>>> + Send>>
        + Send
        + Sync,
>;

/// HTTP Ingress Circuit Builder.
///
/// Wires HTTP inputs to Ranvier Circuits. This is NOT a web server—it's a circuit wiring tool.
///
/// **Ingress is part of Schematic** (separate layer: Ingress → Circuit → Egress)
pub struct HttpIngress<R = ()> {
    /// Bind address (e.g., "127.0.0.1:3000")
    addr: Option<String>,
    /// Routes: (Method, Path) -> Handler
    routes: HashMap<(Method, String), RouteHandler<R>>,
    /// Fallback circuit for unmatched routes
    fallback: Option<RouteHandler<R>>,
    _phantom: std::marker::PhantomData<R>,
}

impl<R> HttpIngress<R>
where
    R: ranvier_core::transition::ResourceRequirement + Clone + Send + Sync + 'static,
{
    /// Create a new empty HttpIngress builder.
    pub fn new() -> Self {
        Self {
            addr: None,
            routes: HashMap::new(),
            fallback: None,
            _phantom: std::marker::PhantomData,
        }
    }

    /// Set the bind address for the server.
    pub fn bind(mut self, addr: impl Into<String>) -> Self {
        self.addr = Some(addr.into());
        self
    }

    /// Register a route with a circuit.
    pub fn route<E>(mut self, path: impl Into<String>, circuit: Axon<(), String, E, R>) -> Self
    where
        E: Send + 'static + std::fmt::Debug,
    {
        let path_str: String = path.into();
        let circuit = Arc::new(circuit);
        let path_for_map = path_str.clone();
        let path_for_handler = path_str;

        let handler: RouteHandler<R> = Arc::new(move |_req: Request<Incoming>, res: &R| {
            let circuit = circuit.clone();
            let res = res.clone(); // R must be Clone
            let path = path_for_handler.clone();

            Box::pin(async move {
                let request_id = uuid::Uuid::new_v4().to_string();
                let span = tracing::info_span!(
                    "HTTPRequest",
                    ranvier.http.method = %Method::GET,
                    ranvier.http.path = %path,
                    ranvier.http.request_id = %request_id
                );

                async move {
                    let mut bus = Bus::new();
                    let result = circuit.execute((), &res, &mut bus).await;

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
                }
                .instrument(span)
                .await
            }) as Pin<Box<dyn Future<Output = Response<Full<Bytes>>> + Send>>
        });

        self.routes.insert((Method::GET, path_for_map), handler);
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
        circuit: Axon<(), String, E, R>,
    ) -> Self
    where
        E: Send + 'static + std::fmt::Debug,
    {
        let path_str: String = path.into();
        let circuit = Arc::new(circuit);
        let path_for_map = path_str.clone();
        let path_for_handler = path_str;
        let method_for_map = method.clone();
        let method_for_handler = method;

        let handler: RouteHandler<R> = Arc::new(move |_req: Request<Incoming>, res: &R| {
            let circuit = circuit.clone();
            let res = res.clone();
            let path = path_for_handler.clone();
            let method = method_for_handler.clone();

            Box::pin(async move {
                let request_id = uuid::Uuid::new_v4().to_string();
                let span = tracing::info_span!(
                    "HTTPRequest",
                    ranvier.http.method = %method,
                    ranvier.http.path = %path,
                    ranvier.http.request_id = %request_id
                );

                async move {
                    let mut bus = Bus::new();
                    let result = circuit.execute((), &res, &mut bus).await;

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
                }
                .instrument(span)
                .await
            }) as Pin<Box<dyn Future<Output = Response<Full<Bytes>>> + Send>>
        });

        self.routes.insert((method_for_map, path_for_map), handler);
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
    pub fn fallback<E>(mut self, circuit: Axon<(), String, E, R>) -> Self
    where
        E: Send + 'static + std::fmt::Debug,
    {
        let circuit = Arc::new(circuit);

        let handler: RouteHandler<R> = Arc::new(move |_req: Request<Incoming>, res: &R| {
            let circuit = circuit.clone();
            let res = res.clone();
            Box::pin(async move {
                let request_id = uuid::Uuid::new_v4().to_string();
                let span = tracing::info_span!(
                    "HTTPRequest",
                    ranvier.http.method = "FALLBACK",
                    ranvier.http.request_id = %request_id
                );

                async move {
                    let mut bus = Bus::new();
                    let result = circuit.execute((), &res, &mut bus).await;

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
                }
                .instrument(span)
                .await
            }) as Pin<Box<dyn Future<Output = Response<Full<Bytes>>> + Send>>
        });

        self.fallback = Some(handler);
        self
    }

    /// Run the HTTP server with required resources.
    pub async fn run(self, resources: R) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let addr_str = self.addr.as_deref().unwrap_or("127.0.0.1:3000");
        let addr: SocketAddr = addr_str.parse()?;

        let routes = Arc::new(self.routes);
        let fallback = self.fallback;
        let resources = Arc::new(resources);

        let listener = TcpListener::bind(addr).await?;
        tracing::info!("Ranvier HTTP Ingress listening on http://{}", addr);

        loop {
            let (stream, _) = listener.accept().await?;
            let io = TokioIo::new(stream);

            let routes = routes.clone();
            let fallback = fallback.clone();
            let resources = resources.clone();

            tokio::task::spawn(async move {
                let resources = resources.clone();
                let service = service_fn(move |req: Request<Incoming>| {
                    let routes = routes.clone();
                    let fallback = fallback.clone();
                    let resources = resources.clone();

                    async move {
                        let method = req.method().clone();
                        let path = req.uri().path().to_string();

                        // Try to find a matching route
                        if let Some(handler) = routes.get(&(method.clone(), path.clone())) {
                            Ok::<_, Infallible>(handler(req, &resources).await)
                        } else if let Some(ref fb) = fallback {
                            Ok(fb(req, &resources).await)
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
    pub fn into_raw_service(self, resources: R) -> RawIngressService<R> {
        let routes = Arc::new(self.routes);
        let fallback = self.fallback;
        let resources = Arc::new(resources);

        RawIngressService {
            routes,
            fallback,
            resources,
        }
    }
}

impl<R> Default for HttpIngress<R>
where
    R: ranvier_core::transition::ResourceRequirement + Clone + Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

/// Internal service type for `into_raw_service()`
#[derive(Clone)]
pub struct RawIngressService<R> {
    routes: Arc<HashMap<(Method, String), RouteHandler<R>>>,
    fallback: Option<RouteHandler<R>>,
    resources: Arc<R>,
}

impl<R> Service<Request<Incoming>> for RawIngressService<R>
where
    R: ranvier_core::transition::ResourceRequirement + Clone + Send + Sync + 'static,
{
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
        let resources = self.resources.clone();

        Box::pin(async move {
            let method = req.method().clone();
            let path = req.uri().path().to_string();

            if let Some(handler) = routes.get(&(method, path)) {
                Ok(handler(req, &resources).await)
            } else if let Some(ref fb) = fallback {
                Ok(fb(req, &resources).await)
            } else {
                Ok(Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Full::new(Bytes::from("Not Found")))
                    .unwrap())
            }
        })
    }
}
