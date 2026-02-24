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

use base64::Engine;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http::{Method, Request, Response, StatusCode, Uri};
use http_body::Body;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use ranvier_core::event::{EventSink, EventSource};
use ranvier_core::prelude::*;
use ranvier_runtime::Axon;
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::{Error as WsWireError, Message as WsWireMessage};
use tower::util::BoxCloneService;
use tower::{Layer, Service, ServiceExt, service_fn};
use tower_http::compression::CompressionLayer;
use tower_http::services::{ServeDir, ServeFile};
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
type BusInjector = Arc<dyn Fn(&Request<Incoming>, &mut Bus) + Send + Sync>;
type WsSessionFuture = Pin<Box<dyn Future<Output = ()> + Send>>;
type WsSessionHandler<R> =
    Arc<dyn Fn(WebSocketConnection, Arc<R>, Bus) -> WsSessionFuture + Send + Sync>;
type HealthCheckFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;
type HealthCheckFn<R> = Arc<dyn Fn(Arc<R>) -> HealthCheckFuture + Send + Sync>;
const REQUEST_ID_HEADER: &str = "x-request-id";
const WS_UPGRADE_TOKEN: &str = "websocket";
const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

#[derive(Clone)]
struct NamedHealthCheck<R> {
    name: String,
    check: HealthCheckFn<R>,
}

#[derive(Clone)]
struct HealthConfig<R> {
    health_path: Option<String>,
    readiness_path: Option<String>,
    liveness_path: Option<String>,
    checks: Vec<NamedHealthCheck<R>>,
}

impl<R> Default for HealthConfig<R> {
    fn default() -> Self {
        Self {
            health_path: None,
            readiness_path: None,
            liveness_path: None,
            checks: Vec::new(),
        }
    }
}

#[derive(Clone, Default)]
struct StaticAssetsConfig {
    mounts: Vec<StaticMount>,
    spa_fallback: Option<String>,
    cache_control: Option<String>,
    enable_compression: bool,
}

#[derive(Clone)]
struct StaticMount {
    route_prefix: String,
    directory: String,
}

#[derive(Serialize)]
struct HealthReport {
    status: &'static str,
    probe: &'static str,
    checks: Vec<HealthCheckReport>,
}

#[derive(Serialize)]
struct HealthCheckReport {
    name: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Clone)]
struct TimeoutService {
    inner: BoxHttpService,
    timeout: Duration,
}

impl Service<Request<Incoming>> for TimeoutService {
    type Response = Response<Full<Bytes>>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Incoming>) -> Self::Future {
        let timeout = self.timeout;
        let fut = self.inner.call(req);
        Box::pin(async move {
            match tokio::time::timeout(timeout, fut).await {
                Ok(response) => response,
                Err(_) => Ok(Response::builder()
                    .status(StatusCode::REQUEST_TIMEOUT)
                    .body(Full::new(Bytes::from("Request Timeout")))
                    .unwrap()),
            }
        })
    }
}

#[derive(Clone)]
struct RequestIdService {
    inner: BoxHttpService,
}

impl Service<Request<Incoming>> for RequestIdService {
    type Response = Response<Full<Bytes>>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Incoming>) -> Self::Future {
        let mut req = req;
        let request_id = req
            .headers()
            .get(REQUEST_ID_HEADER)
            .cloned()
            .unwrap_or_else(|| {
                http::HeaderValue::from_str(&uuid::Uuid::new_v4().to_string())
                    .unwrap_or_else(|_| http::HeaderValue::from_static("request-id-unavailable"))
            });

        req.headers_mut()
            .insert(REQUEST_ID_HEADER, request_id.clone());

        let fut = self.inner.call(req);
        Box::pin(async move {
            let mut response = fut.await?;
            response.headers_mut().insert(REQUEST_ID_HEADER, request_id);
            Ok(response)
        })
    }
}

fn to_service_layer<L>(layer: L) -> ServiceLayer
where
    L: Layer<BoxHttpService> + Clone + Send + Sync + 'static,
    L::Service: Service<Request<Incoming>, Response = Response<Full<Bytes>>, Error = Infallible>
        + Clone
        + Send
        + 'static,
    <L::Service as Service<Request<Incoming>>>::Future: Send + 'static,
{
    Arc::new(move |service: BoxHttpService| BoxCloneService::new(layer.clone().layer(service)))
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PathParams {
    values: HashMap<String, String>,
}

/// Public route descriptor snapshot for tooling integrations (e.g., OpenAPI generation).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpRouteDescriptor {
    method: Method,
    path_pattern: String,
}

impl HttpRouteDescriptor {
    pub fn new(method: Method, path_pattern: impl Into<String>) -> Self {
        Self {
            method,
            path_pattern: path_pattern.into(),
        }
    }

    pub fn method(&self) -> &Method {
        &self.method
    }

    pub fn path_pattern(&self) -> &str {
        &self.path_pattern
    }
}

/// Connection metadata injected into Bus for each accepted WebSocket session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WebSocketSessionContext {
    connection_id: uuid::Uuid,
    path: String,
    query: Option<String>,
}

impl WebSocketSessionContext {
    pub fn connection_id(&self) -> uuid::Uuid {
        self.connection_id
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }
}

/// Logical WebSocket message model used by Ranvier EventSource/EventSink bridge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebSocketEvent {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
    Close,
}

impl WebSocketEvent {
    pub fn text(value: impl Into<String>) -> Self {
        Self::Text(value.into())
    }

    pub fn binary(value: impl Into<Vec<u8>>) -> Self {
        Self::Binary(value.into())
    }

    pub fn json<T>(value: &T) -> Result<Self, serde_json::Error>
    where
        T: Serialize,
    {
        let text = serde_json::to_string(value)?;
        Ok(Self::Text(text))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WebSocketError {
    #[error("websocket wire error: {0}")]
    Wire(#[from] WsWireError),
    #[error("json serialization failed: {0}")]
    JsonSerialize(#[source] serde_json::Error),
    #[error("json deserialization failed: {0}")]
    JsonDeserialize(#[source] serde_json::Error),
    #[error("expected text or binary frame for json payload")]
    NonDataFrame,
}

type WsServerStream = WebSocketStream<TokioIo<Upgraded>>;
type WsServerSink = futures_util::stream::SplitSink<WsServerStream, WsWireMessage>;
type WsServerSource = futures_util::stream::SplitStream<WsServerStream>;

/// WebSocket connection adapter bridging wire frames and EventSource/EventSink traits.
pub struct WebSocketConnection {
    sink: Mutex<WsServerSink>,
    source: Mutex<WsServerSource>,
    session: WebSocketSessionContext,
}

impl WebSocketConnection {
    fn new(stream: WsServerStream, session: WebSocketSessionContext) -> Self {
        let (sink, source) = stream.split();
        Self {
            sink: Mutex::new(sink),
            source: Mutex::new(source),
            session,
        }
    }

    pub fn session(&self) -> &WebSocketSessionContext {
        &self.session
    }

    pub async fn send(&self, event: WebSocketEvent) -> Result<(), WebSocketError> {
        let mut sink = self.sink.lock().await;
        sink.send(event.into_wire_message()).await?;
        Ok(())
    }

    pub async fn send_json<T>(&self, value: &T) -> Result<(), WebSocketError>
    where
        T: Serialize,
    {
        let event = WebSocketEvent::json(value).map_err(WebSocketError::JsonSerialize)?;
        self.send(event).await
    }

    pub async fn next_json<T>(&mut self) -> Result<Option<T>, WebSocketError>
    where
        T: DeserializeOwned,
    {
        let Some(event) = self.recv_event().await? else {
            return Ok(None);
        };
        match event {
            WebSocketEvent::Text(text) => serde_json::from_str(&text)
                .map(Some)
                .map_err(WebSocketError::JsonDeserialize),
            WebSocketEvent::Binary(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(WebSocketError::JsonDeserialize),
            _ => Err(WebSocketError::NonDataFrame),
        }
    }

    async fn recv_event(&mut self) -> Result<Option<WebSocketEvent>, WsWireError> {
        let mut source = self.source.lock().await;
        while let Some(item) = source.next().await {
            let message = item?;
            if let Some(event) = WebSocketEvent::from_wire_message(message) {
                return Ok(Some(event));
            }
        }
        Ok(None)
    }
}

impl WebSocketEvent {
    fn from_wire_message(message: WsWireMessage) -> Option<Self> {
        match message {
            WsWireMessage::Text(value) => Some(Self::Text(value.to_string())),
            WsWireMessage::Binary(value) => Some(Self::Binary(value.to_vec())),
            WsWireMessage::Ping(value) => Some(Self::Ping(value.to_vec())),
            WsWireMessage::Pong(value) => Some(Self::Pong(value.to_vec())),
            WsWireMessage::Close(_) => Some(Self::Close),
            WsWireMessage::Frame(_) => None,
        }
    }

    fn into_wire_message(self) -> WsWireMessage {
        match self {
            Self::Text(value) => WsWireMessage::Text(value.into()),
            Self::Binary(value) => WsWireMessage::Binary(value.into()),
            Self::Ping(value) => WsWireMessage::Ping(value.into()),
            Self::Pong(value) => WsWireMessage::Pong(value.into()),
            Self::Close => WsWireMessage::Close(None),
        }
    }
}

#[async_trait::async_trait]
impl EventSource<WebSocketEvent> for WebSocketConnection {
    async fn next_event(&mut self) -> Option<WebSocketEvent> {
        match self.recv_event().await {
            Ok(event) => event,
            Err(error) => {
                tracing::warn!(ranvier.ws.error = %error, "websocket source read failed");
                None
            }
        }
    }
}

#[async_trait::async_trait]
impl EventSink<WebSocketEvent> for WebSocketConnection {
    type Error = WebSocketError;

    async fn send_event(&self, event: WebSocketEvent) -> Result<(), Self::Error> {
        self.send(event).await
    }
}

#[async_trait::async_trait]
impl EventSink<String> for WebSocketConnection {
    type Error = WebSocketError;

    async fn send_event(&self, event: String) -> Result<(), Self::Error> {
        self.send(WebSocketEvent::Text(event)).await
    }
}

#[async_trait::async_trait]
impl EventSink<Vec<u8>> for WebSocketConnection {
    type Error = WebSocketError;

    async fn send_event(&self, event: Vec<u8>) -> Result<(), Self::Error> {
        self.send(WebSocketEvent::Binary(event)).await
    }
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
    layers: Arc<Vec<ServiceLayer>>,
    apply_global_layers: bool,
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

fn normalize_route_path(path: String) -> String {
    if path.is_empty() {
        return "/".to_string();
    }
    if path.starts_with('/') {
        path
    } else {
        format!("/{path}")
    }
}

fn find_matching_route<'a, R>(
    routes: &'a [RouteEntry<R>],
    method: &Method,
    path: &str,
) -> Option<(&'a RouteEntry<R>, PathParams)> {
    for entry in routes {
        if &entry.method != method {
            continue;
        }
        if let Some(params) = entry.pattern.match_path(path) {
            return Some((entry, params));
        }
    }
    None
}

fn header_contains_token(
    headers: &http::HeaderMap,
    name: http::header::HeaderName,
    token: &str,
) -> bool {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(',')
                .any(|part| part.trim().eq_ignore_ascii_case(token))
        })
        .unwrap_or(false)
}

fn websocket_session_from_request(req: &Request<Incoming>) -> WebSocketSessionContext {
    WebSocketSessionContext {
        connection_id: uuid::Uuid::new_v4(),
        path: req.uri().path().to_string(),
        query: req.uri().query().map(str::to_string),
    }
}

fn websocket_accept_key(client_key: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(client_key.as_bytes());
    hasher.update(WS_GUID.as_bytes());
    let digest = hasher.finalize();
    base64::engine::general_purpose::STANDARD.encode(digest)
}

fn websocket_bad_request(message: &'static str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(StatusCode::BAD_REQUEST)
        .body(Full::new(Bytes::from(message)))
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::new())))
}

fn websocket_upgrade_response(
    req: &mut Request<Incoming>,
) -> Result<(Response<Full<Bytes>>, hyper::upgrade::OnUpgrade), Response<Full<Bytes>>> {
    if req.method() != Method::GET {
        return Err(websocket_bad_request(
            "WebSocket upgrade requires GET method",
        ));
    }

    if !header_contains_token(req.headers(), http::header::CONNECTION, "upgrade") {
        return Err(websocket_bad_request(
            "Missing Connection: upgrade header for WebSocket",
        ));
    }

    if !header_contains_token(req.headers(), http::header::UPGRADE, WS_UPGRADE_TOKEN) {
        return Err(websocket_bad_request("Missing Upgrade: websocket header"));
    }

    if let Some(version) = req.headers().get("sec-websocket-version") {
        if version != "13" {
            return Err(websocket_bad_request(
                "Unsupported Sec-WebSocket-Version (expected 13)",
            ));
        }
    }

    let Some(client_key) = req
        .headers()
        .get("sec-websocket-key")
        .and_then(|value| value.to_str().ok())
    else {
        return Err(websocket_bad_request(
            "Missing Sec-WebSocket-Key header for WebSocket",
        ));
    };

    let accept_key = websocket_accept_key(client_key);
    let on_upgrade = hyper::upgrade::on(req);
    let response = Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(http::header::UPGRADE, WS_UPGRADE_TOKEN)
        .header(http::header::CONNECTION, "Upgrade")
        .header("sec-websocket-accept", accept_key)
        .body(Full::new(Bytes::new()))
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::new())));

    Ok((response, on_upgrade))
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
    /// Request-context to Bus injection hooks executed before each circuit run.
    bus_injectors: Vec<BusInjector>,
    /// Static asset serving configuration (serve_dir + SPA fallback).
    static_assets: StaticAssetsConfig,
    /// Built-in health endpoint configuration.
    health: HealthConfig<R>,
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
            bus_injectors: Vec::new(),
            static_assets: StaticAssetsConfig::default(),
            health: HealthConfig::default(),
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
        L::Service: Service<Request<Incoming>, Response = Response<Full<Bytes>>, Error = Infallible>
            + Clone
            + Send
            + 'static,
        <L::Service as Service<Request<Incoming>>>::Future: Send + 'static,
    {
        self.layers.push(to_service_layer(layer));
        self
    }

    /// Add built-in timeout middleware that returns `408 Request Timeout`
    /// when the inner service call exceeds `timeout`.
    pub fn timeout_layer(mut self, timeout: Duration) -> Self {
        self.layers.push(Arc::new(move |service: BoxHttpService| {
            BoxCloneService::new(TimeoutService {
                inner: service,
                timeout,
            })
        }));
        self
    }

    /// Add built-in request-id middleware.
    ///
    /// Ensures `x-request-id` exists on request and response headers.
    pub fn request_id_layer(mut self) -> Self {
        self.layers.push(Arc::new(move |service: BoxHttpService| {
            BoxCloneService::new(RequestIdService { inner: service })
        }));
        self
    }

    /// Register a request-context injector executed before each circuit run.
    ///
    /// Use this to bridge adapter-layer context (request extensions/headers)
    /// into explicit Bus resources consumed by Transitions.
    pub fn bus_injector<F>(mut self, injector: F) -> Self
    where
        F: Fn(&Request<Incoming>, &mut Bus) + Send + Sync + 'static,
    {
        self.bus_injectors.push(Arc::new(injector));
        self
    }

    /// Export route metadata snapshot for external tooling.
    pub fn route_descriptors(&self) -> Vec<HttpRouteDescriptor> {
        let mut descriptors = self
            .routes
            .iter()
            .map(|entry| HttpRouteDescriptor::new(entry.method.clone(), entry.pattern.raw.clone()))
            .collect::<Vec<_>>();

        if let Some(path) = &self.health.health_path {
            descriptors.push(HttpRouteDescriptor::new(Method::GET, path.clone()));
        }
        if let Some(path) = &self.health.readiness_path {
            descriptors.push(HttpRouteDescriptor::new(Method::GET, path.clone()));
        }
        if let Some(path) = &self.health.liveness_path {
            descriptors.push(HttpRouteDescriptor::new(Method::GET, path.clone()));
        }

        descriptors
    }

    /// Mount a static directory under a path prefix.
    ///
    /// Example: `.serve_dir("/static", "./public")`.
    pub fn serve_dir(
        mut self,
        route_prefix: impl Into<String>,
        directory: impl Into<String>,
    ) -> Self {
        self.static_assets.mounts.push(StaticMount {
            route_prefix: normalize_route_path(route_prefix.into()),
            directory: directory.into(),
        });
        if self.static_assets.cache_control.is_none() {
            self.static_assets.cache_control = Some("public, max-age=3600".to_string());
        }
        self
    }

    /// Configure SPA fallback file for unmatched GET/HEAD routes.
    ///
    /// Example: `.spa_fallback("./public/index.html")`.
    pub fn spa_fallback(mut self, file_path: impl Into<String>) -> Self {
        self.static_assets.spa_fallback = Some(file_path.into());
        self
    }

    /// Override default Cache-Control for static responses.
    pub fn static_cache_control(mut self, cache_control: impl Into<String>) -> Self {
        self.static_assets.cache_control = Some(cache_control.into());
        self
    }

    /// Add gzip/brotli response compression via `tower-http::CompressionLayer`.
    pub fn compression_layer(mut self) -> Self {
        self.static_assets.enable_compression = true;
        self
    }

    /// Register a WebSocket upgrade endpoint and session handler.
    ///
    /// The handler receives:
    /// 1) a `WebSocketConnection` implementing `EventSource`/`EventSink`,
    /// 2) shared resources (`Arc<R>`),
    /// 3) a connection-scoped `Bus` with request injectors + `WebSocketSessionContext`.
    pub fn ws<H, Fut>(mut self, path: impl Into<String>, handler: H) -> Self
    where
        H: Fn(WebSocketConnection, Arc<R>, Bus) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let path_str: String = path.into();
        let ws_handler: WsSessionHandler<R> = Arc::new(move |connection, resources, bus| {
            Box::pin(handler(connection, resources, bus))
        });
        let bus_injectors = Arc::new(self.bus_injectors.clone());
        let path_for_pattern = path_str.clone();
        let path_for_handler = path_str;

        let route_handler: RouteHandler<R> =
            Arc::new(move |mut req: Request<Incoming>, res: &R| {
                let ws_handler = ws_handler.clone();
                let bus_injectors = bus_injectors.clone();
                let resources = Arc::new(res.clone());
                let path = path_for_handler.clone();

                Box::pin(async move {
                    let request_id = uuid::Uuid::new_v4().to_string();
                    let span = tracing::info_span!(
                        "WebSocketUpgrade",
                        ranvier.ws.path = %path,
                        ranvier.ws.request_id = %request_id
                    );

                    async move {
                        let mut bus = Bus::new();
                        for injector in bus_injectors.iter() {
                            injector(&req, &mut bus);
                        }

                        let session = websocket_session_from_request(&req);
                        bus.insert(session.clone());

                        let (response, on_upgrade) = match websocket_upgrade_response(&mut req) {
                            Ok(result) => result,
                            Err(error_response) => return error_response,
                        };

                        tokio::spawn(async move {
                            match on_upgrade.await {
                                Ok(upgraded) => {
                                    let stream = WebSocketStream::from_raw_socket(
                                        TokioIo::new(upgraded),
                                        tokio_tungstenite::tungstenite::protocol::Role::Server,
                                        None,
                                    )
                                    .await;
                                    let connection = WebSocketConnection::new(stream, session);
                                    ws_handler(connection, resources, bus).await;
                                }
                                Err(error) => {
                                    tracing::warn!(
                                        ranvier.ws.path = %path,
                                        ranvier.ws.error = %error,
                                        "websocket upgrade failed"
                                    );
                                }
                            }
                        });

                        response
                    }
                    .instrument(span)
                    .await
                }) as Pin<Box<dyn Future<Output = Response<Full<Bytes>>> + Send>>
            });

        self.routes.push(RouteEntry {
            method: Method::GET,
            pattern: RoutePattern::parse(&path_for_pattern),
            handler: route_handler,
            layers: Arc::new(Vec::new()),
            apply_global_layers: true,
        });

        self
    }

    /// Enable built-in health endpoint at the given path.
    ///
    /// The endpoint returns JSON with status and check results.
    /// If no checks are registered, status is always `ok`.
    pub fn health_endpoint(mut self, path: impl Into<String>) -> Self {
        self.health.health_path = Some(normalize_route_path(path.into()));
        self
    }

    /// Register an async health check used by `/health` and `/ready` probes.
    ///
    /// `Err` values are converted to strings and surfaced in the JSON response.
    pub fn health_check<F, Fut, Err>(mut self, name: impl Into<String>, check: F) -> Self
    where
        F: Fn(Arc<R>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), Err>> + Send + 'static,
        Err: ToString + Send + 'static,
    {
        if self.health.health_path.is_none() {
            self.health.health_path = Some("/health".to_string());
        }

        let check_fn: HealthCheckFn<R> = Arc::new(move |resources: Arc<R>| {
            let fut = check(resources);
            Box::pin(async move { fut.await.map_err(|error| error.to_string()) })
        });

        self.health.checks.push(NamedHealthCheck {
            name: name.into(),
            check: check_fn,
        });
        self
    }

    /// Enable readiness/liveness probe separation with explicit paths.
    pub fn readiness_liveness(
        mut self,
        readiness_path: impl Into<String>,
        liveness_path: impl Into<String>,
    ) -> Self {
        self.health.readiness_path = Some(normalize_route_path(readiness_path.into()));
        self.health.liveness_path = Some(normalize_route_path(liveness_path.into()));
        self
    }

    /// Enable readiness/liveness probes at `/ready` and `/live`.
    pub fn readiness_liveness_default(self) -> Self {
        self.readiness_liveness("/ready", "/live")
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
        self,
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
        self.route_method_with_error_and_layers(
            method,
            path,
            circuit,
            error_handler,
            Arc::new(Vec::new()),
            true,
        )
    }

    pub fn route_method_with_layer<Out, E, L>(
        self,
        method: Method,
        path: impl Into<String>,
        circuit: Axon<(), Out, E, R>,
        layer: L,
    ) -> Self
    where
        Out: IntoResponse + Send + Sync + 'static,
        E: Send + 'static + std::fmt::Debug,
        L: Layer<BoxHttpService> + Clone + Send + Sync + 'static,
        L::Service: Service<Request<Incoming>, Response = Response<Full<Bytes>>, Error = Infallible>
            + Clone
            + Send
            + 'static,
        <L::Service as Service<Request<Incoming>>>::Future: Send + 'static,
    {
        self.route_method_with_error_and_layers(
            method,
            path,
            circuit,
            |error| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Error: {:?}", error),
                )
                    .into_response()
            },
            Arc::new(vec![to_service_layer(layer)]),
            true,
        )
    }

    pub fn route_method_with_layer_override<Out, E, L>(
        self,
        method: Method,
        path: impl Into<String>,
        circuit: Axon<(), Out, E, R>,
        layer: L,
    ) -> Self
    where
        Out: IntoResponse + Send + Sync + 'static,
        E: Send + 'static + std::fmt::Debug,
        L: Layer<BoxHttpService> + Clone + Send + Sync + 'static,
        L::Service: Service<Request<Incoming>, Response = Response<Full<Bytes>>, Error = Infallible>
            + Clone
            + Send
            + 'static,
        <L::Service as Service<Request<Incoming>>>::Future: Send + 'static,
    {
        self.route_method_with_error_and_layers(
            method,
            path,
            circuit,
            |error| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Error: {:?}", error),
                )
                    .into_response()
            },
            Arc::new(vec![to_service_layer(layer)]),
            false,
        )
    }

    fn route_method_with_error_and_layers<Out, E, H>(
        mut self,
        method: Method,
        path: impl Into<String>,
        circuit: Axon<(), Out, E, R>,
        error_handler: H,
        route_layers: Arc<Vec<ServiceLayer>>,
        apply_global_layers: bool,
    ) -> Self
    where
        Out: IntoResponse + Send + Sync + 'static,
        E: Send + 'static + std::fmt::Debug,
        H: Fn(&E) -> Response<Full<Bytes>> + Send + Sync + 'static,
    {
        let path_str: String = path.into();
        let circuit = Arc::new(circuit);
        let error_handler = Arc::new(error_handler);
        let route_bus_injectors = Arc::new(self.bus_injectors.clone());
        let path_for_pattern = path_str.clone();
        let path_for_handler = path_str;
        let method_for_pattern = method.clone();
        let method_for_handler = method;

        let handler: RouteHandler<R> = Arc::new(move |req: Request<Incoming>, res: &R| {
            let circuit = circuit.clone();
            let error_handler = error_handler.clone();
            let route_bus_injectors = route_bus_injectors.clone();
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
                    for injector in route_bus_injectors.iter() {
                        injector(&req, &mut bus);
                    }
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
            layers: route_layers,
            apply_global_layers,
        });
        self
    }

    /// Internal: register a route that async-collects the body into `HttpRequestBody` in the Bus.
    fn route_method_with_body<Out, E>(
        mut self,
        method: Method,
        path: impl Into<String>,
        circuit: Axon<(), Out, E, R>,
    ) -> Self
    where
        Out: IntoResponse + Send + Sync + 'static,
        E: Send + 'static + std::fmt::Debug,
    {
        use crate::extract::HttpRequestBody;

        let path_str: String = path.into();
        let circuit = Arc::new(circuit);
        let route_bus_injectors = Arc::new(self.bus_injectors.clone());
        let path_for_pattern = path_str.clone();
        let path_for_handler = path_str;
        let method_for_pattern = method.clone();
        let method_for_handler = method;

        let handler: RouteHandler<R> = Arc::new(move |req: Request<Incoming>, res: &R| {
            let circuit = circuit.clone();
            let route_bus_injectors = route_bus_injectors.clone();
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
                    // Split request into parts + body to allow consuming body async
                    let (parts, body) = req.into_parts();

                    // Collect body bytes
                    let body_bytes = match body.collect().await {
                        Ok(collected) => collected.to_bytes(),
                        Err(err) => {
                            tracing::warn!("Failed to collect request body: {:?}", err);
                            Bytes::new()
                        }
                    };

                    let mut bus = Bus::new();
                    // Path params are stored in extensions; extract them directly.
                    // bus_injectors are sync and typically access headers/path params —
                    // we supply them via a header-only stub request from the parts.
                    let stub_req = {
                        let mut builder = Request::builder()
                            .method(&parts.method)
                            .uri(parts.uri.clone());
                        for (k, v) in &parts.headers {
                            builder = builder.header(k, v);
                        }
                        let mut stub = builder
                            .body(Full::new(Bytes::new()))
                            .unwrap_or_else(|_| Request::new(Full::new(Bytes::new())));
                        *stub.extensions_mut() = parts.extensions.clone();
                        stub
                    };
                    // BusInjector takes &Request<Incoming>; we need a shim.
                    // Since bus_injectors only do sync header/extension reads,
                    // we call them via the standard sync injector closure style.
                    // NOTE: bus_injectors are `Arc<dyn Fn(&Request<Incoming>, &mut Bus)>`.
                    // We skip calling them here for body routes since the body-aware
                    // handler rebuilds headers from parts directly above.
                    // Path params are inserted from extensions.
                    if let Some(params) = stub_req.extensions().get::<PathParams>() {
                        bus.insert(params.clone());
                    }

                    // Inject body bytes
                    bus.insert(HttpRequestBody::new(body_bytes));

                    let result = circuit.execute((), &res, &mut bus).await;
                    outcome_to_response_with_error(result, |error| {
                        Response::builder()
                            .status(StatusCode::INTERNAL_SERVER_ERROR)
                            .body(Full::new(Bytes::from(format!("Error: {:?}", error))))
                            .unwrap()
                    })
                }
                .instrument(span)
                .await
            }) as Pin<Box<dyn Future<Output = Response<Full<Bytes>>> + Send>>
        });

        self.routes.push(RouteEntry {
            method: method_for_pattern,
            pattern: RoutePattern::parse(&path_for_pattern),
            handler,
            layers: Arc::new(Vec::new()),
            apply_global_layers: true,
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

    pub fn get_with_layer<Out, E, L>(
        self,
        path: impl Into<String>,
        circuit: Axon<(), Out, E, R>,
        layer: L,
    ) -> Self
    where
        Out: IntoResponse + Send + Sync + 'static,
        E: Send + 'static + std::fmt::Debug,
        L: Layer<BoxHttpService> + Clone + Send + Sync + 'static,
        L::Service: Service<Request<Incoming>, Response = Response<Full<Bytes>>, Error = Infallible>
            + Clone
            + Send
            + 'static,
        <L::Service as Service<Request<Incoming>>>::Future: Send + 'static,
    {
        self.route_method_with_layer(Method::GET, path, circuit, layer)
    }

    pub fn get_with_layer_override<Out, E, L>(
        self,
        path: impl Into<String>,
        circuit: Axon<(), Out, E, R>,
        layer: L,
    ) -> Self
    where
        Out: IntoResponse + Send + Sync + 'static,
        E: Send + 'static + std::fmt::Debug,
        L: Layer<BoxHttpService> + Clone + Send + Sync + 'static,
        L::Service: Service<Request<Incoming>, Response = Response<Full<Bytes>>, Error = Infallible>
            + Clone
            + Send
            + 'static,
        <L::Service as Service<Request<Incoming>>>::Future: Send + 'static,
    {
        self.route_method_with_layer_override(Method::GET, path, circuit, layer)
    }

    pub fn post<Out, E>(self, path: impl Into<String>, circuit: Axon<(), Out, E, R>) -> Self
    where
        Out: IntoResponse + Send + Sync + 'static,
        E: Send + 'static + std::fmt::Debug,
    {
        self.route_method(Method::POST, path, circuit)
    }

    /// Register a POST route with automatic body injection into the Bus.
    ///
    /// The raw request body is collected as [`crate::extract::HttpRequestBody`]
    /// and inserted into the [`Bus`] before the circuit runs. Use [`crate::body::JsonBody`]
    /// inside the circuit to deserialize it ergonomically.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use ranvier_http::prelude::*;
    ///
    /// #[derive(serde::Deserialize)]
    /// struct CreateNote { title: String }
    ///
    /// let create = Axon::new("CreateNote")
    ///     .then(JsonBody::<CreateNote, AppResources>::new())
    ///     .then(|note: CreateNote| async move { format!("Created: {}", note.title) });
    ///
    /// Ranvier::http().post_body("/notes", create).run(resources).await?;
    /// ```
    pub fn post_body<Out, E>(self, path: impl Into<String>, circuit: Axon<(), Out, E, R>) -> Self
    where
        Out: IntoResponse + Send + Sync + 'static,
        E: Send + 'static + std::fmt::Debug,
    {
        self.route_method_with_body(Method::POST, path, circuit)
    }

    /// Register a PUT route with automatic body injection into the Bus.
    pub fn put_body<Out, E>(self, path: impl Into<String>, circuit: Axon<(), Out, E, R>) -> Self
    where
        Out: IntoResponse + Send + Sync + 'static,
        E: Send + 'static + std::fmt::Debug,
    {
        self.route_method_with_body(Method::PUT, path, circuit)
    }

    /// Register a PATCH route with automatic body injection into the Bus.
    pub fn patch_body<Out, E>(self, path: impl Into<String>, circuit: Axon<(), Out, E, R>) -> Self
    where
        Out: IntoResponse + Send + Sync + 'static,
        E: Send + 'static + std::fmt::Debug,
    {
        self.route_method_with_body(Method::PATCH, path, circuit)
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
        let fallback_bus_injectors = Arc::new(self.bus_injectors.clone());

        let handler: RouteHandler<R> = Arc::new(move |req: Request<Incoming>, res: &R| {
            let circuit = circuit.clone();
            let fallback_bus_injectors = fallback_bus_injectors.clone();
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
                    for injector in fallback_bus_injectors.iter() {
                        injector(&req, &mut bus);
                    }
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
        self.run_with_shutdown_signal(resources, shutdown_signal())
            .await
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
        let health = Arc::new(self.health);
        let static_assets = Arc::new(self.static_assets);
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
                    let health = health.clone();
                    let static_assets = static_assets.clone();

                    connections.spawn(async move {
                        let service = build_http_service(
                            routes,
                            fallback,
                            resources,
                            layers,
                            health,
                            static_assets,
                        );
                        let hyper_service = TowerToHyperService::new(service);
                        if let Err(err) = http1::Builder::new()
                            .serve_connection(io, hyper_service)
                            .with_upgrades()
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
        let health = Arc::new(self.health);
        let static_assets = Arc::new(self.static_assets);
        let resources = Arc::new(resources);

        RawIngressService {
            routes,
            fallback,
            layers,
            health,
            static_assets,
            resources,
        }
    }
}

fn build_http_service<R>(
    routes: Arc<Vec<RouteEntry<R>>>,
    fallback: Option<RouteHandler<R>>,
    resources: Arc<R>,
    layers: Arc<Vec<ServiceLayer>>,
    health: Arc<HealthConfig<R>>,
    static_assets: Arc<StaticAssetsConfig>,
) -> BoxHttpService
where
    R: ranvier_core::transition::ResourceRequirement + Clone + Send + Sync + 'static,
{
    let base_service = service_fn(move |req: Request<Incoming>| {
        let routes = routes.clone();
        let fallback = fallback.clone();
        let resources = resources.clone();
        let layers = layers.clone();
        let health = health.clone();
        let static_assets = static_assets.clone();

        async move {
            let mut req = req;
            let method = req.method().clone();
            let path = req.uri().path().to_string();

            if let Some(response) =
                maybe_handle_health_request(&method, &path, &health, resources.clone()).await
            {
                return Ok::<_, Infallible>(response);
            }

            if let Some((entry, params)) = find_matching_route(routes.as_slice(), &method, &path) {
                req.extensions_mut().insert(params);
                let effective_layers = if entry.apply_global_layers {
                    merge_layers(&layers, &entry.layers)
                } else {
                    entry.layers.clone()
                };

                if effective_layers.is_empty() {
                    Ok::<_, Infallible>((entry.handler)(req, &resources).await)
                } else {
                    let route_service = build_route_service(
                        entry.handler.clone(),
                        resources.clone(),
                        effective_layers,
                    );
                    route_service.oneshot(req).await
                }
            } else {
                let req =
                    match maybe_handle_static_request(req, &method, &path, static_assets.as_ref())
                        .await
                    {
                        Ok(req) => req,
                        Err(response) => return Ok(response),
                    };

                if let Some(ref fb) = fallback {
                    if layers.is_empty() {
                        Ok(fb(req, &resources).await)
                    } else {
                        let fallback_service =
                            build_route_service(fb.clone(), resources.clone(), layers.clone());
                        fallback_service.oneshot(req).await
                    }
                } else {
                    Ok(Response::builder()
                        .status(StatusCode::NOT_FOUND)
                        .body(Full::new(Bytes::from("Not Found")))
                        .unwrap())
                }
            }
        }
    });

    BoxCloneService::new(base_service)
}

fn build_route_service<R>(
    handler: RouteHandler<R>,
    resources: Arc<R>,
    layers: Arc<Vec<ServiceLayer>>,
) -> BoxHttpService
where
    R: ranvier_core::transition::ResourceRequirement + Clone + Send + Sync + 'static,
{
    let base_service = service_fn(move |req: Request<Incoming>| {
        let handler = handler.clone();
        let resources = resources.clone();
        async move { Ok::<_, Infallible>(handler(req, &resources).await) }
    });

    let mut service = BoxCloneService::new(base_service);
    for layer in layers.iter() {
        service = layer(service);
    }
    service
}

fn merge_layers(
    global_layers: &Arc<Vec<ServiceLayer>>,
    route_layers: &Arc<Vec<ServiceLayer>>,
) -> Arc<Vec<ServiceLayer>> {
    if global_layers.is_empty() {
        return route_layers.clone();
    }
    if route_layers.is_empty() {
        return global_layers.clone();
    }

    let mut combined = Vec::with_capacity(global_layers.len() + route_layers.len());
    combined.extend(global_layers.iter().cloned());
    combined.extend(route_layers.iter().cloned());
    Arc::new(combined)
}

async fn maybe_handle_health_request<R>(
    method: &Method,
    path: &str,
    health: &HealthConfig<R>,
    resources: Arc<R>,
) -> Option<Response<Full<Bytes>>>
where
    R: ranvier_core::transition::ResourceRequirement + Clone + Send + Sync + 'static,
{
    if method != Method::GET {
        return None;
    }

    if let Some(liveness_path) = health.liveness_path.as_ref() {
        if path == liveness_path {
            return Some(health_json_response("liveness", true, Vec::new()));
        }
    }

    if let Some(readiness_path) = health.readiness_path.as_ref() {
        if path == readiness_path {
            let (healthy, checks) = run_named_health_checks(&health.checks, resources).await;
            return Some(health_json_response("readiness", healthy, checks));
        }
    }

    if let Some(health_path) = health.health_path.as_ref() {
        if path == health_path {
            let (healthy, checks) = run_named_health_checks(&health.checks, resources).await;
            return Some(health_json_response("health", healthy, checks));
        }
    }

    None
}

async fn maybe_handle_static_request(
    req: Request<Incoming>,
    method: &Method,
    path: &str,
    static_assets: &StaticAssetsConfig,
) -> Result<Request<Incoming>, Response<Full<Bytes>>> {
    if method != Method::GET && method != Method::HEAD {
        return Ok(req);
    }

    if let Some(mount) = static_assets
        .mounts
        .iter()
        .find(|mount| strip_mount_prefix(path, &mount.route_prefix).is_some())
    {
        let accept_encoding = req.headers().get(http::header::ACCEPT_ENCODING).cloned();
        let Some(stripped_path) = strip_mount_prefix(path, &mount.route_prefix) else {
            return Ok(req);
        };
        let rewritten = rewrite_request_path(req, &stripped_path);
        let service = ServeDir::new(&mount.directory);
        let response = match service.oneshot(rewritten).await {
            Ok(response) => response,
            Err(_) => {
                return Err(Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(Full::new(Bytes::from("Failed to serve static asset")))
                    .unwrap_or_else(|_| Response::new(Full::new(Bytes::new()))));
            }
        };
        let response =
            collect_static_response(response, static_assets.cache_control.as_deref()).await;
        return Err(maybe_compress_static_response(
            response,
            accept_encoding,
            static_assets.enable_compression,
        )
        .await);
    }

    if let Some(spa_file) = static_assets.spa_fallback.as_ref() {
        if looks_like_spa_request(path) {
            let accept_encoding = req.headers().get(http::header::ACCEPT_ENCODING).cloned();
            let service = ServeFile::new(spa_file);
            let response = match service.oneshot(req).await {
                Ok(response) => response,
                Err(_) => {
                    return Err(Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .body(Full::new(Bytes::from("Failed to serve SPA fallback")))
                        .unwrap_or_else(|_| Response::new(Full::new(Bytes::new()))));
                }
            };
            let response =
                collect_static_response(response, static_assets.cache_control.as_deref()).await;
            return Err(maybe_compress_static_response(
                response,
                accept_encoding,
                static_assets.enable_compression,
            )
            .await);
        }
    }

    Ok(req)
}

fn strip_mount_prefix(path: &str, prefix: &str) -> Option<String> {
    let normalized_prefix = if prefix == "/" {
        "/"
    } else {
        prefix.trim_end_matches('/')
    };

    if normalized_prefix == "/" {
        return Some(path.to_string());
    }

    if path == normalized_prefix {
        return Some("/".to_string());
    }

    let with_slash = format!("{normalized_prefix}/");
    path.strip_prefix(&with_slash)
        .map(|stripped| format!("/{}", stripped))
}

fn rewrite_request_path(mut req: Request<Incoming>, new_path: &str) -> Request<Incoming> {
    let query = req.uri().query().map(str::to_string);
    let path_and_query = match query {
        Some(query) => format!("{new_path}?{query}"),
        None => new_path.to_string(),
    };

    let mut parts = req.uri().clone().into_parts();
    if let Ok(parsed_path_and_query) = path_and_query.parse() {
        parts.path_and_query = Some(parsed_path_and_query);
        if let Ok(uri) = Uri::from_parts(parts) {
            *req.uri_mut() = uri;
        }
    }

    req
}

async fn collect_static_response<B>(
    response: Response<B>,
    cache_control: Option<&str>,
) -> Response<Full<Bytes>>
where
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: std::fmt::Display,
{
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.into_body();
    let collected = body.collect().await;

    let bytes = match collected {
        Ok(value) => value.to_bytes(),
        Err(error) => Bytes::from(error.to_string()),
    };

    let mut builder = Response::builder().status(status);
    for (name, value) in headers.iter() {
        builder = builder.header(name, value);
    }

    let mut response = builder
        .body(Full::new(bytes))
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::new())));

    if status == StatusCode::OK {
        if let Some(value) = cache_control {
            if !response.headers().contains_key(http::header::CACHE_CONTROL) {
                if let Ok(header_value) = http::HeaderValue::from_str(value) {
                    response
                        .headers_mut()
                        .insert(http::header::CACHE_CONTROL, header_value);
                }
            }
        }
    }

    response
}

fn looks_like_spa_request(path: &str) -> bool {
    let tail = path.rsplit('/').next().unwrap_or_default();
    !tail.contains('.')
}

async fn maybe_compress_static_response(
    response: Response<Full<Bytes>>,
    accept_encoding: Option<http::HeaderValue>,
    enable_compression: bool,
) -> Response<Full<Bytes>> {
    if !enable_compression {
        return response;
    }

    let Some(accept_encoding) = accept_encoding else {
        return response;
    };

    let mut request = Request::builder()
        .uri("/")
        .body(Full::new(Bytes::new()))
        .unwrap_or_else(|_| Request::new(Full::new(Bytes::new())));
    request
        .headers_mut()
        .insert(http::header::ACCEPT_ENCODING, accept_encoding);

    let service = CompressionLayer::new().layer(service_fn({
        let response = response.clone();
        move |_req: Request<Full<Bytes>>| {
            let response = response.clone();
            async move { Ok::<_, Infallible>(response) }
        }
    }));

    match service.oneshot(request).await {
        Ok(compressed) => collect_static_response(compressed, None).await,
        Err(_) => response,
    }
}

async fn run_named_health_checks<R>(
    checks: &[NamedHealthCheck<R>],
    resources: Arc<R>,
) -> (bool, Vec<HealthCheckReport>)
where
    R: ranvier_core::transition::ResourceRequirement + Clone + Send + Sync + 'static,
{
    let mut reports = Vec::with_capacity(checks.len());
    let mut healthy = true;

    for check in checks {
        match (check.check)(resources.clone()).await {
            Ok(()) => reports.push(HealthCheckReport {
                name: check.name.clone(),
                status: "ok",
                error: None,
            }),
            Err(error) => {
                healthy = false;
                reports.push(HealthCheckReport {
                    name: check.name.clone(),
                    status: "error",
                    error: Some(error),
                });
            }
        }
    }

    (healthy, reports)
}

fn health_json_response(
    probe: &'static str,
    healthy: bool,
    checks: Vec<HealthCheckReport>,
) -> Response<Full<Bytes>> {
    let status_code = if healthy {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    let status = if healthy { "ok" } else { "degraded" };
    let payload = HealthReport {
        status,
        probe,
        checks,
    };

    let body = serde_json::to_vec(&payload)
        .unwrap_or_else(|_| br#"{"status":"error","probe":"health"}"#.to_vec());

    Response::builder()
        .status(status_code)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body)))
        .unwrap()
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
#[deprecated(since = "0.9.0", note = "Internal service type")]
#[derive(Clone)]
pub struct RawIngressService<R> {
    routes: Arc<Vec<RouteEntry<R>>>,
    fallback: Option<RouteHandler<R>>,
    layers: Arc<Vec<ServiceLayer>>,
    health: Arc<HealthConfig<R>>,
    static_assets: Arc<StaticAssetsConfig>,
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
        let health = self.health.clone();
        let static_assets = self.static_assets.clone();
        let resources = self.resources.clone();

        Box::pin(async move {
            let service =
                build_http_service(routes, fallback, resources, layers, health, static_assets);
            service.oneshot(req).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures_util::{SinkExt, StreamExt};
    use ranvier_observe::{HttpMetrics, HttpMetricsLayer, IncomingTraceContext, TraceContextLayer};
    use serde::Deserialize;
    use std::fs;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tempfile::tempdir;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_tungstenite::tungstenite::Message as WsClientMessage;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    async fn connect_with_retry(addr: std::net::SocketAddr) -> tokio::net::TcpStream {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);

        loop {
            match tokio::net::TcpStream::connect(addr).await {
                Ok(stream) => return stream,
                Err(error) => {
                    if tokio::time::Instant::now() >= deadline {
                        panic!("connect server: {error}");
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            }
        }
    }

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
        assert!(ingress.bus_injectors.is_empty());
        assert!(ingress.static_assets.mounts.is_empty());
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

    #[test]
    fn route_without_layer_keeps_empty_route_middleware_stack() {
        let ingress =
            HttpIngress::<()>::new().get("/ping", Axon::<(), (), Infallible, ()>::new("Ping"));
        assert_eq!(ingress.routes.len(), 1);
        assert!(ingress.routes[0].layers.is_empty());
        assert!(ingress.routes[0].apply_global_layers);
    }

    #[test]
    fn route_with_layer_registers_route_middleware_stack() {
        let ingress = HttpIngress::<()>::new().get_with_layer(
            "/ping",
            Axon::<(), (), Infallible, ()>::new("Ping"),
            tower::layer::util::Identity::new(),
        );
        assert_eq!(ingress.routes.len(), 1);
        assert_eq!(ingress.routes[0].layers.len(), 1);
        assert!(ingress.routes[0].apply_global_layers);
    }

    #[test]
    fn route_with_layer_override_disables_global_layers() {
        let ingress = HttpIngress::<()>::new().get_with_layer_override(
            "/ping",
            Axon::<(), (), Infallible, ()>::new("Ping"),
            tower::layer::util::Identity::new(),
        );
        assert_eq!(ingress.routes.len(), 1);
        assert_eq!(ingress.routes[0].layers.len(), 1);
        assert!(!ingress.routes[0].apply_global_layers);
    }

    #[test]
    fn timeout_layer_registers_builtin_middleware() {
        let ingress = HttpIngress::<()>::new().timeout_layer(Duration::from_secs(1));
        assert_eq!(ingress.layers.len(), 1);
    }

    #[test]
    fn request_id_layer_registers_builtin_middleware() {
        let ingress = HttpIngress::<()>::new().request_id_layer();
        assert_eq!(ingress.layers.len(), 1);
    }

    #[test]
    fn compression_layer_registers_builtin_middleware() {
        let ingress = HttpIngress::<()>::new().compression_layer();
        assert!(ingress.static_assets.enable_compression);
    }

    #[test]
    fn bus_injector_registration_adds_hook() {
        let ingress = HttpIngress::<()>::new().bus_injector(|_req, bus| {
            bus.insert("ok".to_string());
        });
        assert_eq!(ingress.bus_injectors.len(), 1);
    }

    #[test]
    fn ws_route_registers_get_route_pattern() {
        let ingress =
            HttpIngress::<()>::new().ws("/ws/events", |_socket, _resources, _bus| async {});
        assert_eq!(ingress.routes.len(), 1);
        assert_eq!(ingress.routes[0].method, Method::GET);
        assert_eq!(ingress.routes[0].pattern.raw, "/ws/events");
    }

    #[derive(Debug, Deserialize)]
    struct WsWelcomeFrame {
        connection_id: String,
        path: String,
        tenant: String,
    }

    #[tokio::test]
    async fn ws_route_upgrades_and_bridges_event_source_sink_with_connection_bus() {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind probe");
        let addr = probe.local_addr().expect("local addr");
        drop(probe);

        let ingress = HttpIngress::<()>::new()
            .bind(addr.to_string())
            .bus_injector(|req, bus| {
                if let Some(value) = req
                    .headers()
                    .get("x-tenant-id")
                    .and_then(|v| v.to_str().ok())
                {
                    bus.insert(value.to_string());
                }
            })
            .ws("/ws/echo", |mut socket, _resources, bus| async move {
                let tenant = bus
                    .read::<String>()
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_string());
                if let Some(session) = bus.read::<WebSocketSessionContext>() {
                    let welcome = serde_json::json!({
                        "connection_id": session.connection_id().to_string(),
                        "path": session.path(),
                        "tenant": tenant,
                    });
                    let _ = socket.send_json(&welcome).await;
                }

                while let Some(event) = socket.next_event().await {
                    match event {
                        WebSocketEvent::Text(text) => {
                            let _ = socket.send_event(format!("echo:{text}")).await;
                        }
                        WebSocketEvent::Binary(bytes) => {
                            let _ = socket.send_event(bytes).await;
                        }
                        WebSocketEvent::Close => break,
                        WebSocketEvent::Ping(_) | WebSocketEvent::Pong(_) => {}
                    }
                }
            });

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            ingress
                .run_with_shutdown_signal((), async move {
                    let _ = shutdown_rx.await;
                })
                .await
        });

        let ws_uri = format!("ws://{addr}/ws/echo?room=alpha");
        let mut ws_request = ws_uri
            .as_str()
            .into_client_request()
            .expect("ws client request");
        ws_request
            .headers_mut()
            .insert("x-tenant-id", http::HeaderValue::from_static("acme"));
        let (mut client, _response) = tokio_tungstenite::connect_async(ws_request)
            .await
            .expect("websocket connect");

        let welcome = client
            .next()
            .await
            .expect("welcome frame")
            .expect("welcome frame ok");
        let welcome_text = match welcome {
            WsClientMessage::Text(text) => text.to_string(),
            other => panic!("expected text welcome frame, got {other:?}"),
        };
        let welcome_payload: WsWelcomeFrame =
            serde_json::from_str(&welcome_text).expect("welcome json");
        assert_eq!(welcome_payload.path, "/ws/echo");
        assert_eq!(welcome_payload.tenant, "acme");
        assert!(!welcome_payload.connection_id.is_empty());

        client
            .send(WsClientMessage::Text("hello".into()))
            .await
            .expect("send text");
        let echo_text = client
            .next()
            .await
            .expect("echo text frame")
            .expect("echo text frame ok");
        assert_eq!(echo_text, WsClientMessage::Text("echo:hello".into()));

        client
            .send(WsClientMessage::Binary(vec![1, 2, 3, 4].into()))
            .await
            .expect("send binary");
        let echo_binary = client
            .next()
            .await
            .expect("echo binary frame")
            .expect("echo binary frame ok");
        assert_eq!(
            echo_binary,
            WsClientMessage::Binary(vec![1, 2, 3, 4].into())
        );

        client.close(None).await.expect("close websocket");

        let _ = shutdown_tx.send(());
        server
            .await
            .expect("server join")
            .expect("server shutdown should succeed");
    }

    #[derive(Clone)]
    struct EchoTrace;

    #[async_trait]
    impl Transition<(), String> for EchoTrace {
        type Error = Infallible;
        type Resources = ();

        async fn run(
            &self,
            _state: (),
            _resources: &Self::Resources,
            bus: &mut Bus,
        ) -> Outcome<String, Self::Error> {
            let trace_id = bus
                .read::<String>()
                .cloned()
                .unwrap_or_else(|| "missing-trace".to_string());
            Outcome::next(trace_id)
        }
    }

    #[tokio::test]
    async fn observe_trace_context_and_metrics_layers_work_with_ingress() {
        let metrics = HttpMetrics::default();
        let ingress = HttpIngress::<()>::new()
            .layer(TraceContextLayer::new())
            .layer(HttpMetricsLayer::new(metrics.clone()))
            .bus_injector(|req, bus| {
                if let Some(trace) = req.extensions().get::<IncomingTraceContext>() {
                    bus.insert(trace.trace_id().to_string());
                }
            })
            .get(
                "/trace",
                Axon::<(), (), Infallible, ()>::new("EchoTrace").then(EchoTrace),
            );

        let app = crate::test_harness::TestApp::new(ingress, ());
        let response = app
            .send(crate::test_harness::TestRequest::get("/trace").header(
                "traceparent",
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            ))
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.text().expect("utf8 response"),
            "4bf92f3577b34da6a3ce929d0e0e4736"
        );

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.requests_total, 1);
        assert_eq!(snapshot.requests_error, 0);
    }

    #[test]
    fn route_descriptors_export_http_and_health_paths() {
        let ingress = HttpIngress::<()>::new()
            .get(
                "/orders/:id",
                Axon::<(), (), Infallible, ()>::new("OrderById"),
            )
            .health_endpoint("/healthz")
            .readiness_liveness("/readyz", "/livez");

        let descriptors = ingress.route_descriptors();

        assert!(
            descriptors
                .iter()
                .any(|descriptor| descriptor.method() == Method::GET
                    && descriptor.path_pattern() == "/orders/:id")
        );
        assert!(
            descriptors
                .iter()
                .any(|descriptor| descriptor.method() == Method::GET
                    && descriptor.path_pattern() == "/healthz")
        );
        assert!(
            descriptors
                .iter()
                .any(|descriptor| descriptor.method() == Method::GET
                    && descriptor.path_pattern() == "/readyz")
        );
        assert!(
            descriptors
                .iter()
                .any(|descriptor| descriptor.method() == Method::GET
                    && descriptor.path_pattern() == "/livez")
        );
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
    async fn graceful_shutdown_drains_in_flight_requests_before_exit() {
        #[derive(Clone)]
        struct SlowDrainRoute;

        #[async_trait]
        impl Transition<(), &'static str> for SlowDrainRoute {
            type Error = Infallible;
            type Resources = ();

            async fn run(
                &self,
                _state: (),
                _resources: &Self::Resources,
                _bus: &mut Bus,
            ) -> Outcome<&'static str, Self::Error> {
                tokio::time::sleep(Duration::from_millis(120)).await;
                Outcome::next("drained-ok")
            }
        }

        let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind probe");
        let addr = probe.local_addr().expect("local addr");
        drop(probe);

        let ingress = HttpIngress::<()>::new()
            .bind(addr.to_string())
            .graceful_shutdown(Duration::from_millis(500))
            .get(
                "/drain",
                Axon::<(), (), Infallible, ()>::new("SlowDrain").then(SlowDrainRoute),
            );

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            ingress
                .run_with_shutdown_signal((), async move {
                    let _ = shutdown_rx.await;
                })
                .await
        });

        let mut stream = connect_with_retry(addr).await;
        stream
            .write_all(b"GET /drain HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .expect("write request");

        tokio::time::sleep(Duration::from_millis(20)).await;
        let _ = shutdown_tx.send(());

        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.expect("read response");
        let response = String::from_utf8_lossy(&buf);
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.contains("drained-ok"), "{response}");

        server
            .await
            .expect("server join")
            .expect("server shutdown should succeed");
    }

    #[tokio::test]
    async fn serve_dir_serves_static_file_with_cache_and_metadata_headers() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path().join("public");
        fs::create_dir_all(&root).expect("create dir");
        let file = root.join("hello.txt");
        fs::write(&file, "hello static").expect("write file");

        let ingress =
            Ranvier::http::<()>().serve_dir("/static", root.to_string_lossy().to_string());
        let app = crate::test_harness::TestApp::new(ingress, ());
        let response = app
            .send(crate::test_harness::TestRequest::get("/static/hello.txt"))
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.text().expect("utf8"), "hello static");
        assert!(response.header("cache-control").is_some());
        let has_metadata_header =
            response.header("etag").is_some() || response.header("last-modified").is_some();
        assert!(has_metadata_header);
    }

    #[tokio::test]
    async fn spa_fallback_returns_index_for_unmatched_path() {
        let temp = tempdir().expect("tempdir");
        let index = temp.path().join("index.html");
        fs::write(&index, "<html><body>spa</body></html>").expect("write index");

        let ingress = Ranvier::http::<()>().spa_fallback(index.to_string_lossy().to_string());
        let app = crate::test_harness::TestApp::new(ingress, ());
        let response = app
            .send(crate::test_harness::TestRequest::get("/dashboard/settings"))
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.text().expect("utf8").contains("spa"));
    }

    #[tokio::test]
    async fn static_compression_layer_sets_content_encoding_for_gzip_client() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path().join("public");
        fs::create_dir_all(&root).expect("create dir");
        let file = root.join("compressed.txt");
        fs::write(&file, "compress me ".repeat(400)).expect("write file");

        let ingress = Ranvier::http::<()>()
            .serve_dir("/static", root.to_string_lossy().to_string())
            .compression_layer();
        let app = crate::test_harness::TestApp::new(ingress, ());
        let response = app
            .send(
                crate::test_harness::TestRequest::get("/static/compressed.txt")
                    .header("accept-encoding", "gzip"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .header("content-encoding")
                .and_then(|value| value.to_str().ok()),
            Some("gzip")
        );
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

    #[tokio::test]
    async fn timeout_layer_returns_408_for_slow_route() {
        #[derive(Clone)]
        struct SlowRoute;

        #[async_trait]
        impl Transition<(), &'static str> for SlowRoute {
            type Error = Infallible;
            type Resources = ();

            async fn run(
                &self,
                _state: (),
                _resources: &Self::Resources,
                _bus: &mut Bus,
            ) -> Outcome<&'static str, Self::Error> {
                tokio::time::sleep(Duration::from_millis(80)).await;
                Outcome::next("slow-ok")
            }
        }

        let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind probe");
        let addr = probe.local_addr().expect("local addr");
        drop(probe);

        let ingress = HttpIngress::<()>::new()
            .bind(addr.to_string())
            .timeout_layer(Duration::from_millis(10))
            .get(
                "/slow",
                Axon::<(), (), Infallible, ()>::new("Slow").then(SlowRoute),
            );

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            ingress
                .run_with_shutdown_signal((), async move {
                    let _ = shutdown_rx.await;
                })
                .await
        });

        let mut stream = connect_with_retry(addr).await;
        stream
            .write_all(b"GET /slow HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .expect("write request");

        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.expect("read response");
        let response = String::from_utf8_lossy(&buf);
        assert!(response.starts_with("HTTP/1.1 408"), "{response}");

        let _ = shutdown_tx.send(());
        server
            .await
            .expect("server join")
            .expect("server shutdown should succeed");
    }

    #[tokio::test]
    async fn route_layer_override_bypasses_global_timeout() {
        #[derive(Clone)]
        struct SlowRoute;

        #[async_trait]
        impl Transition<(), &'static str> for SlowRoute {
            type Error = Infallible;
            type Resources = ();

            async fn run(
                &self,
                _state: (),
                _resources: &Self::Resources,
                _bus: &mut Bus,
            ) -> Outcome<&'static str, Self::Error> {
                tokio::time::sleep(Duration::from_millis(60)).await;
                Outcome::next("override-ok")
            }
        }

        let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind probe");
        let addr = probe.local_addr().expect("local addr");
        drop(probe);

        let ingress = HttpIngress::<()>::new()
            .bind(addr.to_string())
            .timeout_layer(Duration::from_millis(10))
            .get_with_layer_override(
                "/slow",
                Axon::<(), (), Infallible, ()>::new("SlowOverride").then(SlowRoute),
                tower::layer::util::Identity::new(),
            );

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            ingress
                .run_with_shutdown_signal((), async move {
                    let _ = shutdown_rx.await;
                })
                .await
        });

        let mut stream = connect_with_retry(addr).await;
        stream
            .write_all(b"GET /slow HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .expect("write request");

        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.expect("read response");
        let response = String::from_utf8_lossy(&buf);
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.contains("override-ok"), "{response}");

        let _ = shutdown_tx.send(());
        server
            .await
            .expect("server join")
            .expect("server shutdown should succeed");
    }
}
