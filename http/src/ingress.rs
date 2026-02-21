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
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use ranvier_core::prelude::*;
use ranvier_runtime::Axon;
use std::collections::HashMap;
use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tower::util::BoxCloneService;
use tower::{Layer, Service, ServiceExt, service_fn};
use tracing::Instrument;

use crate::response::{IntoResponse, outcome_to_response_with_error};

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

type BoxHttpService = BoxCloneService<Request<Incoming>, Response<Full<Bytes>>, Infallible>;
type ServiceLayer = Arc<dyn Fn(BoxHttpService) -> BoxHttpService + Send + Sync>;
type LifecycleHook = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PathParams {
    values: HashMap<String, String>,
}

impl PathParams {
    pub fn new(values: HashMap<String, String>) -> Self {
        Self { values }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    pub fn as_map(&self) -> &HashMap<String, String> {
        &self.values
    }

    pub fn into_inner(self) -> HashMap<String, String> {
        self.values
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RouteSegment {
    Static(String),
    Param(String),
    Wildcard(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RoutePattern {
    raw: String,
    segments: Vec<RouteSegment>,
}

impl RoutePattern {
    fn parse(path: &str) -> Self {
        let segments = path_segments(path)
            .into_iter()
            .map(|segment| {
                if let Some(name) = segment.strip_prefix(':') {
                    if !name.is_empty() {
                        return RouteSegment::Param(name.to_string());
                    }
                }
                if let Some(name) = segment.strip_prefix('*') {
                    if !name.is_empty() {
                        return RouteSegment::Wildcard(name.to_string());
                    }
                }
                RouteSegment::Static(segment.to_string())
            })
            .collect();

        Self {
            raw: path.to_string(),
            segments,
        }
    }

    fn match_path(&self, path: &str) -> Option<PathParams> {
        let mut params = HashMap::new();
        let path_segments = path_segments(path);
        let mut pattern_index = 0usize;
        let mut path_index = 0usize;

        while pattern_index < self.segments.len() {
            match &self.segments[pattern_index] {
                RouteSegment::Static(expected) => {
                    let actual = path_segments.get(path_index)?;
                    if actual != expected {
                        return None;
                    }
                    pattern_index += 1;
                    path_index += 1;
                }
                RouteSegment::Param(name) => {
                    let actual = path_segments.get(path_index)?;
                    params.insert(name.clone(), (*actual).to_string());
                    pattern_index += 1;
                    path_index += 1;
                }
                RouteSegment::Wildcard(name) => {
                    let remaining = path_segments[path_index..].join("/");
                    params.insert(name.clone(), remaining);
                    pattern_index += 1;
                    path_index = path_segments.len();
                    break;
                }
            }
        }

        if pattern_index == self.segments.len() && path_index == path_segments.len() {
            Some(PathParams::new(params))
        } else {
            None
        }
    }
}

#[derive(Clone)]
struct RouteEntry<R> {
    method: Method,
    pattern: RoutePattern,
    handler: RouteHandler<R>,
}

fn path_segments(path: &str) -> Vec<&str> {
    if path == "/" {
        return Vec::new();
    }

    path.trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect()
}

fn find_matching_route<'a, R>(
    routes: &'a [RouteEntry<R>],
    method: &Method,
    path: &str,
) -> Option<(&'a RouteHandler<R>, PathParams)> {
    for entry in routes {
        if &entry.method != method {
            continue;
        }
        if let Some(params) = entry.pattern.match_path(path) {
            return Some((&entry.handler, params));
        }
    }
    None
}

/// HTTP Ingress Circuit Builder.
///
/// Wires HTTP inputs to Ranvier Circuits. This is NOT a web server—it's a circuit wiring tool.
///
/// **Ingress is part of Schematic** (separate layer: Ingress → Circuit → Egress)
pub struct HttpIngress<R = ()> {
    /// Bind address (e.g., "127.0.0.1:3000")
    addr: Option<String>,
    /// Routes: (Method, RoutePattern, Handler)
    routes: Vec<RouteEntry<R>>,
    /// Fallback circuit for unmatched routes
    fallback: Option<RouteHandler<R>>,
    /// Global middleware layers (LIFO execution on request path).
    layers: Vec<ServiceLayer>,
    /// Lifecycle callback invoked after listener bind succeeds.
    on_start: Option<LifecycleHook>,
    /// Lifecycle callback invoked when graceful shutdown finishes.
    on_shutdown: Option<LifecycleHook>,
    /// Maximum time to wait for in-flight requests to drain.
    graceful_shutdown_timeout: Duration,
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
            routes: Vec::new(),
            fallback: None,
            layers: Vec::new(),
            on_start: None,
            on_shutdown: None,
            graceful_shutdown_timeout: Duration::from_secs(30),
            _phantom: std::marker::PhantomData,
        }
    }

    /// Set the bind address for the server.
    pub fn bind(mut self, addr: impl Into<String>) -> Self {
        self.addr = Some(addr.into());
        self
    }

    /// Register a lifecycle callback invoked when the server starts listening.
    pub fn on_start<F>(mut self, callback: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.on_start = Some(Arc::new(callback));
        self
    }

    /// Register a lifecycle callback invoked after graceful shutdown completes.
    pub fn on_shutdown<F>(mut self, callback: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.on_shutdown = Some(Arc::new(callback));
        self
    }

    /// Configure graceful shutdown timeout for in-flight request draining.
    pub fn graceful_shutdown(mut self, timeout: Duration) -> Self {
        self.graceful_shutdown_timeout = timeout;
        self
    }

    /// Add a global Tower layer to the ingress service stack.
    ///
    /// Layers execute in LIFO order on the request path:
    /// the last layer added is the first to receive the request.
    pub fn layer<L>(mut self, layer: L) -> Self
    where
        L: Layer<BoxHttpService> + Clone + Send + Sync + 'static,
        L::Service:
            Service<Request<Incoming>, Response = Response<Full<Bytes>>, Error = Infallible>
                + Clone
                + Send
                + 'static,
        <L::Service as Service<Request<Incoming>>>::Future: Send + 'static,
    {
        self.layers
            .push(Arc::new(move |service: BoxHttpService| {
                BoxCloneService::new(layer.clone().layer(service))
            }));
        self
    }

    /// Register a route with GET method.
    pub fn route<Out, E>(self, path: impl Into<String>, circuit: Axon<(), Out, E, R>) -> Self
    where
        Out: IntoResponse + Send + Sync + 'static,
        E: Send + 'static + std::fmt::Debug,
    {
        self.route_method(Method::GET, path, circuit)
    }
    /// Register a route with a specific HTTP method.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// Ranvier::http()
    ///     .route_method(Method::POST, "/users", create_user_circuit)
    /// ```
    pub fn route_method<Out, E>(
        self,
        method: Method,
        path: impl Into<String>,
        circuit: Axon<(), Out, E, R>,
    ) -> Self
    where
        Out: IntoResponse + Send + Sync + 'static,
        E: Send + 'static + std::fmt::Debug,
    {
        self.route_method_with_error(method, path, circuit, |error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Error: {:?}", error),
            )
                .into_response()
        })
    }

    pub fn route_method_with_error<Out, E, H>(
        mut self,
        method: Method,
        path: impl Into<String>,
        circuit: Axon<(), Out, E, R>,
        error_handler: H,
    ) -> Self
    where
        Out: IntoResponse + Send + Sync + 'static,
        E: Send + 'static + std::fmt::Debug,
        H: Fn(&E) -> Response<Full<Bytes>> + Send + Sync + 'static,
    {
        let path_str: String = path.into();
        let circuit = Arc::new(circuit);
        let error_handler = Arc::new(error_handler);
        let path_for_pattern = path_str.clone();
        let path_for_handler = path_str;
        let method_for_pattern = method.clone();
        let method_for_handler = method;

        let handler: RouteHandler<R> = Arc::new(move |_req: Request<Incoming>, res: &R| {
            let circuit = circuit.clone();
            let error_handler = error_handler.clone();
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
                    outcome_to_response_with_error(result, |error| error_handler(error))
                }
                .instrument(span)
                .await
            }) as Pin<Box<dyn Future<Output = Response<Full<Bytes>>> + Send>>
        });

        self.routes.push(RouteEntry {
            method: method_for_pattern,
            pattern: RoutePattern::parse(&path_for_pattern),
            handler,
        });
        self
    }

    pub fn get<Out, E>(self, path: impl Into<String>, circuit: Axon<(), Out, E, R>) -> Self
    where
        Out: IntoResponse + Send + Sync + 'static,
        E: Send + 'static + std::fmt::Debug,
    {
        self.route_method(Method::GET, path, circuit)
    }

    pub fn get_with_error<Out, E, H>(
        self,
        path: impl Into<String>,
        circuit: Axon<(), Out, E, R>,
        error_handler: H,
    ) -> Self
    where
        Out: IntoResponse + Send + Sync + 'static,
        E: Send + 'static + std::fmt::Debug,
        H: Fn(&E) -> Response<Full<Bytes>> + Send + Sync + 'static,
    {
        self.route_method_with_error(Method::GET, path, circuit, error_handler)
    }

    pub fn post<Out, E>(self, path: impl Into<String>, circuit: Axon<(), Out, E, R>) -> Self
    where
        Out: IntoResponse + Send + Sync + 'static,
        E: Send + 'static + std::fmt::Debug,
    {
        self.route_method(Method::POST, path, circuit)
    }

    pub fn put<Out, E>(self, path: impl Into<String>, circuit: Axon<(), Out, E, R>) -> Self
    where
        Out: IntoResponse + Send + Sync + 'static,
        E: Send + 'static + std::fmt::Debug,
    {
        self.route_method(Method::PUT, path, circuit)
    }

    pub fn delete<Out, E>(self, path: impl Into<String>, circuit: Axon<(), Out, E, R>) -> Self
    where
        Out: IntoResponse + Send + Sync + 'static,
        E: Send + 'static + std::fmt::Debug,
    {
        self.route_method(Method::DELETE, path, circuit)
    }

    pub fn patch<Out, E>(self, path: impl Into<String>, circuit: Axon<(), Out, E, R>) -> Self
    where
        Out: IntoResponse + Send + Sync + 'static,
        E: Send + 'static + std::fmt::Debug,
    {
        self.route_method(Method::PATCH, path, circuit)
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
    pub fn fallback<Out, E>(mut self, circuit: Axon<(), Out, E, R>) -> Self
    where
        Out: IntoResponse + Send + Sync + 'static,
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
                        Outcome::Next(output) => {
                            let mut response = output.into_response();
                            *response.status_mut() = StatusCode::NOT_FOUND;
                            response
                        }
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
        self.run_with_shutdown_signal(resources, shutdown_signal()).await
    }

    async fn run_with_shutdown_signal<S>(
        self,
        resources: R,
        shutdown_signal: S,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
    where
        S: Future<Output = ()> + Send,
    {
        let addr_str = self.addr.as_deref().unwrap_or("127.0.0.1:3000");
        let addr: SocketAddr = addr_str.parse()?;

        let routes = Arc::new(self.routes);
        let fallback = self.fallback;
        let layers = Arc::new(self.layers);
        let on_start = self.on_start;
        let on_shutdown = self.on_shutdown;
        let graceful_shutdown_timeout = self.graceful_shutdown_timeout;
        let resources = Arc::new(resources);

        let listener = TcpListener::bind(addr).await?;
        tracing::info!("Ranvier HTTP Ingress listening on http://{}", addr);
        if let Some(callback) = on_start.as_ref() {
            callback();
        }

        tokio::pin!(shutdown_signal);
        let mut connections = tokio::task::JoinSet::new();

        loop {
            tokio::select! {
                _ = &mut shutdown_signal => {
                    tracing::info!("Shutdown signal received. Draining in-flight connections.");
                    break;
                }
                accept_result = listener.accept() => {
                    let (stream, _) = accept_result?;
                    let io = TokioIo::new(stream);

                    let routes = routes.clone();
                    let fallback = fallback.clone();
                    let resources = resources.clone();
                    let layers = layers.clone();

                    connections.spawn(async move {
                        let service = build_http_service(routes, fallback, resources, layers);
                        let hyper_service = TowerToHyperService::new(service);
                        if let Err(err) = http1::Builder::new()
                            .serve_connection(io, hyper_service)
                            .await
                        {
                            tracing::error!("Error serving connection: {:?}", err);
                        }
                    });
                }
                Some(join_result) = connections.join_next(), if !connections.is_empty() => {
                    if let Err(err) = join_result {
                        tracing::warn!("Connection task join error: {:?}", err);
                    }
                }
            }
        }

        let _timed_out = drain_connections(&mut connections, graceful_shutdown_timeout).await;

        drop(resources);
        if let Some(callback) = on_shutdown.as_ref() {
            callback();
        }

        Ok(())
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
        let layers = Arc::new(self.layers);
        let resources = Arc::new(resources);

        RawIngressService {
            routes,
            fallback,
            layers,
            resources,
        }
    }
}

fn build_http_service<R>(
    routes: Arc<Vec<RouteEntry<R>>>,
    fallback: Option<RouteHandler<R>>,
    resources: Arc<R>,
    layers: Arc<Vec<ServiceLayer>>,
) -> BoxHttpService
where
    R: ranvier_core::transition::ResourceRequirement + Clone + Send + Sync + 'static,
{
    let base_service = service_fn(move |req: Request<Incoming>| {
        let routes = routes.clone();
        let fallback = fallback.clone();
        let resources = resources.clone();

        async move {
            let mut req = req;
            let method = req.method().clone();
            let path = req.uri().path().to_string();

            if let Some((handler, params)) = find_matching_route(routes.as_slice(), &method, &path)
            {
                req.extensions_mut().insert(params);
                Ok::<_, Infallible>(handler(req, &resources).await)
            } else if let Some(ref fb) = fallback {
                Ok(fb(req, &resources).await)
            } else {
                Ok(Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Full::new(Bytes::from("Not Found")))
                    .unwrap())
            }
        }
    });

    let mut service = BoxCloneService::new(base_service);
    for layer in layers.iter() {
        service = layer(service);
    }
    service
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = terminate.recv() => {}
                }
            }
            Err(err) => {
                tracing::warn!("Failed to install SIGTERM handler: {:?}", err);
                if let Err(ctrl_c_err) = tokio::signal::ctrl_c().await {
                    tracing::warn!("Failed to listen for Ctrl+C: {:?}", ctrl_c_err);
                }
            }
        }
    }

    #[cfg(not(unix))]
    {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::warn!("Failed to listen for Ctrl+C: {:?}", err);
        }
    }
}

async fn drain_connections(
    connections: &mut tokio::task::JoinSet<()>,
    graceful_shutdown_timeout: Duration,
) -> bool {
    if connections.is_empty() {
        return false;
    }

    let drain_result = tokio::time::timeout(graceful_shutdown_timeout, async {
        while let Some(join_result) = connections.join_next().await {
            if let Err(err) = join_result {
                tracing::warn!("Connection task join error during shutdown: {:?}", err);
            }
        }
    })
    .await;

    if drain_result.is_err() {
        tracing::warn!(
            "Graceful shutdown timeout reached ({:?}). Aborting remaining connections.",
            graceful_shutdown_timeout
        );
        connections.abort_all();
        while let Some(join_result) = connections.join_next().await {
            if let Err(err) = join_result {
                tracing::warn!("Connection task abort join error: {:?}", err);
            }
        }
        true
    } else {
        false
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
    routes: Arc<Vec<RouteEntry<R>>>,
    fallback: Option<RouteHandler<R>>,
    layers: Arc<Vec<ServiceLayer>>,
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
        let layers = self.layers.clone();
        let resources = self.resources.clone();

        Box::pin(async move {
            let service = build_http_service(routes, fallback, resources, layers);
            service.oneshot(req).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn route_pattern_matches_static_path() {
        let pattern = RoutePattern::parse("/orders/list");
        let params = pattern.match_path("/orders/list").expect("should match");
        assert!(params.into_inner().is_empty());
    }

    #[test]
    fn route_pattern_matches_param_segments() {
        let pattern = RoutePattern::parse("/orders/:id/items/:item_id");
        let params = pattern
            .match_path("/orders/42/items/sku-123")
            .expect("should match");
        assert_eq!(params.get("id"), Some("42"));
        assert_eq!(params.get("item_id"), Some("sku-123"));
    }

    #[test]
    fn route_pattern_matches_wildcard_segment() {
        let pattern = RoutePattern::parse("/assets/*path");
        let params = pattern
            .match_path("/assets/css/theme/light.css")
            .expect("should match");
        assert_eq!(params.get("path"), Some("css/theme/light.css"));
    }

    #[test]
    fn route_pattern_rejects_non_matching_path() {
        let pattern = RoutePattern::parse("/orders/:id");
        assert!(pattern.match_path("/users/42").is_none());
    }

    #[test]
    fn graceful_shutdown_timeout_defaults_to_30_seconds() {
        let ingress = HttpIngress::<()>::new();
        assert_eq!(ingress.graceful_shutdown_timeout, Duration::from_secs(30));
        assert!(ingress.layers.is_empty());
        assert!(ingress.on_start.is_none());
        assert!(ingress.on_shutdown.is_none());
    }

    #[test]
    fn layer_registration_stacks_globally() {
        let ingress = HttpIngress::<()>::new()
            .layer(tower::layer::util::Identity::new())
            .layer(tower::layer::util::Identity::new());
        assert_eq!(ingress.layers.len(), 2);
    }

    #[test]
    fn layer_accepts_tower_http_cors_layer() {
        let ingress = HttpIngress::<()>::new().layer(tower_http::cors::CorsLayer::permissive());
        assert_eq!(ingress.layers.len(), 1);
    }

    #[tokio::test]
    async fn lifecycle_hooks_fire_on_start_and_shutdown() {
        let started = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::new(AtomicBool::new(false));

        let started_flag = started.clone();
        let shutdown_flag = shutdown.clone();

        let ingress = HttpIngress::<()>::new()
            .bind("127.0.0.1:0")
            .on_start(move || {
                started_flag.store(true, Ordering::SeqCst);
            })
            .on_shutdown(move || {
                shutdown_flag.store(true, Ordering::SeqCst);
            })
            .graceful_shutdown(Duration::from_millis(50));

        ingress
            .run_with_shutdown_signal((), async {
                tokio::time::sleep(Duration::from_millis(20)).await;
            })
            .await
            .expect("server should exit gracefully");

        assert!(started.load(Ordering::SeqCst));
        assert!(shutdown.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn drain_connections_completes_before_timeout() {
        let mut connections = tokio::task::JoinSet::new();
        connections.spawn(async {
            tokio::time::sleep(Duration::from_millis(20)).await;
        });

        let timed_out = drain_connections(&mut connections, Duration::from_millis(200)).await;
        assert!(!timed_out);
        assert!(connections.is_empty());
    }

    #[tokio::test]
    async fn drain_connections_times_out_and_aborts() {
        let mut connections = tokio::task::JoinSet::new();
        connections.spawn(async {
            tokio::time::sleep(Duration::from_secs(10)).await;
        });

        let timed_out = drain_connections(&mut connections, Duration::from_millis(10)).await;
        assert!(timed_out);
        assert!(connections.is_empty());
    }

}
