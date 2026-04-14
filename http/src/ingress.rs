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
use http::{Method, Request, Response, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
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
use tracing::Instrument;

use crate::guard_integration::{
    GuardExec, GuardIntegration, PreflightConfig, RegisteredGuard, ResponseBodyTransformFn,
    ResponseExtractorFn,
};
use crate::response::{HttpResponse, IntoResponse, json_error_response, outcome_to_json_response, outcome_to_response_with_error};

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
    dyn Fn(http::request::Parts, &R) -> Pin<Box<dyn Future<Output = HttpResponse> + Send>>
        + Send
        + Sync,
>;

/// Type-erased cloneable HTTP service (replaces tower::util::BoxCloneService).
#[derive(Clone)]
struct BoxService(
    Arc<
        dyn Fn(Request<Incoming>) -> Pin<Box<dyn Future<Output = Result<HttpResponse, Infallible>> + Send>>
            + Send
            + Sync,
    >,
);

impl BoxService {
    fn new<F, Fut>(f: F) -> Self
    where
        F: Fn(Request<Incoming>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<HttpResponse, Infallible>> + Send + 'static,
    {
        Self(Arc::new(move |req| Box::pin(f(req))))
    }

    fn call(&self, req: Request<Incoming>) -> Pin<Box<dyn Future<Output = Result<HttpResponse, Infallible>> + Send>> {
        (self.0)(req)
    }
}

impl hyper::service::Service<Request<Incoming>> for BoxService {
    type Response = HttpResponse;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<HttpResponse, Infallible>> + Send>>;

    fn call(&self, req: Request<Incoming>) -> Self::Future {
        (self.0)(req)
    }
}

type BoxHttpService = BoxService;
type ServiceLayer = Arc<dyn Fn(BoxHttpService) -> BoxHttpService + Send + Sync>;
type LifecycleHook = Arc<dyn Fn() + Send + Sync>;
type BusInjector = Arc<dyn Fn(&http::request::Parts, &mut Bus) + Send + Sync + 'static>;
type WsSessionFuture = Pin<Box<dyn Future<Output = ()> + Send>>;
type WsSessionHandler<R> =
    Arc<dyn Fn(WebSocketConnection, Arc<R>, Bus) -> WsSessionFuture + Send + Sync>;
type HealthCheckFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;
type HealthCheckFn<R> = Arc<dyn Fn(Arc<R>) -> HealthCheckFuture + Send + Sync>;
const REQUEST_ID_HEADER: &str = "x-request-id";
const WS_UPGRADE_TOKEN: &str = "websocket";
const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

fn should_suppress_incomplete_message_error() -> bool {
    matches!(
        std::env::var("RANVIER_SUPPRESS_INCOMPLETE_MESSAGE_ERROR"),
        Ok(value) if matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES")
    )
}

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
    /// Default index filename for directory requests (e.g., "index.html").
    directory_index: Option<String>,
    /// When true, detect hashed filenames (e.g., `app.a1b2c3.js`) and apply
    /// `Cache-Control: public, max-age=31536000, immutable`.
    immutable_cache: bool,
    /// When true, check for pre-compressed `.br` / `.gz` variants before serving.
    serve_precompressed: bool,
    /// When true, support `Range: bytes=X-Y` requests with 206 Partial Content.
    enable_range_requests: bool,
}

#[derive(Clone)]
struct StaticMount {
    route_prefix: String,
    directory: String,
}

/// TLS configuration for HTTPS serving.
#[cfg(feature = "tls")]
#[derive(Clone)]
struct TlsAcceptorConfig {
    cert_path: String,
    key_path: String,
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

fn timeout_middleware(timeout: Duration) -> ServiceLayer {
    Arc::new(move |inner: BoxHttpService| {
        BoxService::new(move |req: Request<Incoming>| {
            let inner = inner.clone();
            async move {
                match tokio::time::timeout(timeout, inner.call(req)).await {
                    Ok(response) => response,
                    Err(_) => Ok(Response::builder()
                        .status(StatusCode::REQUEST_TIMEOUT)
                        .body(
                            Full::new(Bytes::from("Request Timeout"))
                                .map_err(|never| match never {})
                                .boxed(),
                        )
                        .expect("valid HTTP response construction")),
                }
            }
        })
    })
}

fn request_id_middleware() -> ServiceLayer {
    Arc::new(move |inner: BoxHttpService| {
        BoxService::new(move |req: Request<Incoming>| {
            let inner = inner.clone();
            async move {
                let mut req = req;
                let request_id = req
                    .headers()
                    .get(REQUEST_ID_HEADER)
                    .cloned()
                    .unwrap_or_else(|| {
                        http::HeaderValue::from_str(&uuid::Uuid::new_v4().to_string())
                            .unwrap_or_else(|_| {
                                http::HeaderValue::from_static("request-id-unavailable")
                            })
                    });
                req.headers_mut()
                    .insert(REQUEST_ID_HEADER, request_id.clone());
                let mut response = inner.call(req).await?;
                response
                    .headers_mut()
                    .insert(REQUEST_ID_HEADER, request_id);
                Ok(response)
            }
        })
    })
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PathParams {
    values: HashMap<String, String>,
}

/// Public route descriptor snapshot for tooling integrations (e.g., OpenAPI generation).
#[derive(Clone, Debug)]
pub struct HttpRouteDescriptor {
    method: Method,
    path_pattern: String,
    /// JSON Schema for the request body (auto-captured from `post_typed` etc.).
    pub body_schema: Option<serde_json::Value>,
}

impl HttpRouteDescriptor {
    pub fn new(method: Method, path_pattern: impl Into<String>) -> Self {
        Self {
            method,
            path_pattern: path_pattern.into(),
            body_schema: None,
        }
    }

    pub fn method(&self) -> &Method {
        &self.method
    }

    pub fn path_pattern(&self) -> &str {
        &self.path_pattern
    }

    /// Returns the JSON Schema for the request body, if available.
    ///
    /// Automatically populated when routes are registered via `post_typed`,
    /// `put_typed`, or `patch_typed` (requires `T: JsonSchema`).
    pub fn body_schema(&self) -> Option<&serde_json::Value> {
        self.body_schema.as_ref()
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
            Self::Text(value) => WsWireMessage::Text(value),
            Self::Binary(value) => WsWireMessage::Binary(value),
            Self::Ping(value) => WsWireMessage::Ping(value),
            Self::Pong(value) => WsWireMessage::Pong(value),
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

/// Parsed query string parameters extracted from the request URI.
///
/// Automatically injected into the Bus by `HttpIngress` for every request.
/// Use `bus.read::<QueryParams>()` inside transitions to access query parameters.
///
/// # Example
///
/// ```rust,ignore
/// let page = bus.read::<QueryParams>()
///     .and_then(|q| q.get_parsed::<i64>("page"))
///     .unwrap_or(1);
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QueryParams {
    values: HashMap<String, String>,
}

impl QueryParams {
    /// Create QueryParams from a raw query string (e.g. `"page=1&limit=20"`).
    pub fn from_query(query: &str) -> Self {
        let values = query
            .split('&')
            .filter(|s| !s.is_empty())
            .filter_map(|pair| {
                let mut parts = pair.splitn(2, '=');
                let key = parts.next()?.to_string();
                let value = parts.next().unwrap_or("").to_string();
                Some((key, value))
            })
            .collect();
        Self { values }
    }

    /// Get a raw string value by key.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    /// Parse a query parameter value as type `T`.
    ///
    /// Returns `None` if the key is absent or parsing fails.
    pub fn get_parsed<T: std::str::FromStr>(&self, key: &str) -> Option<T> {
        self.values.get(key).and_then(|v| v.parse().ok())
    }

    /// Get a parsed value, or return `default` if absent or unparseable.
    pub fn get_or<T: std::str::FromStr>(&self, key: &str, default: T) -> T {
        self.get_parsed(key).unwrap_or(default)
    }

    /// Check if a key exists in the query parameters.
    pub fn contains(&self, key: &str) -> bool {
        self.values.contains_key(key)
    }

    /// Get reference to the underlying map.
    pub fn as_map(&self) -> &HashMap<String, String> {
        &self.values
    }
}

impl PathParams {
    pub fn new(values: HashMap<String, String>) -> Self {
        Self { values }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    /// Parse a path parameter value as type `T`.
    ///
    /// Returns `None` if the key is absent or parsing fails.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let id: Option<Uuid> = params.get_parsed::<Uuid>("id");
    /// ```
    pub fn get_parsed<T: std::str::FromStr>(&self, key: &str) -> Option<T> {
        self.values.get(key).and_then(|v| v.parse().ok())
    }

    pub fn as_map(&self) -> &HashMap<String, String> {
        &self.values
    }

    pub fn into_inner(self) -> HashMap<String, String> {
        self.values
    }
}

/// Inject `QueryParams` into the Bus from request parts.
///
/// Called automatically by `HttpIngress` before user-provided `bus_injector`.
fn inject_query_params(parts: &http::request::Parts, bus: &mut ranvier_core::bus::Bus) {
    if let Some(query) = parts.uri.query() {
        bus.insert(QueryParams::from_query(query));
    } else {
        bus.insert(QueryParams::default());
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

/// Body bytes extracted from `Request<Incoming>` for typed route handlers.
/// Stored in `Parts::extensions` so handlers can access the raw body.
#[derive(Clone)]
struct BodyBytes(Bytes);

#[derive(Clone)]
struct RouteEntry<R> {
    method: Method,
    pattern: RoutePattern,
    handler: RouteHandler<R>,
    layers: Arc<Vec<ServiceLayer>>,
    apply_global_layers: bool,
    /// When true, the dispatch layer reads the request body and stores it
    /// in `Parts::extensions` as `BodyBytes` before calling the handler.
    needs_body: bool,
    /// JSON Schema for the request body type (from `post_typed` etc.).
    body_schema: Option<serde_json::Value>,
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
        if entry.method != *method {
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

fn websocket_session_from_request<B>(req: &Request<B>) -> WebSocketSessionContext {
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

fn websocket_bad_request(message: &'static str) -> HttpResponse {
    Response::builder()
        .status(StatusCode::BAD_REQUEST)
        .body(
            Full::new(Bytes::from(message))
                .map_err(|never| match never {})
                .boxed(),
        )
        .unwrap_or_else(|_| {
            Response::new(
                Full::new(Bytes::new())
                    .map_err(|never| match never {})
                    .boxed(),
            )
        })
}

fn websocket_upgrade_response<B>(
    req: &mut Request<B>,
) -> Result<(HttpResponse, hyper::upgrade::OnUpgrade), HttpResponse> {
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
        .body(
            Full::new(Bytes::new())
                .map_err(|never| match never {})
                .boxed(),
        )
        .unwrap_or_else(|_| {
            Response::new(
                Full::new(Bytes::new())
                    .map_err(|never| match never {})
                    .boxed(),
            )
        });

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
    #[cfg(feature = "http3")]
    http3_config: Option<crate::http3::Http3Config>,
    #[cfg(feature = "http3")]
    alt_svc_h3_port: Option<u16>,
    /// TLS configuration (feature-gated: `tls`)
    #[cfg(feature = "tls")]
    tls_config: Option<TlsAcceptorConfig>,
    /// Features: enable active intervention system routes
    active_intervention: bool,
    /// Optional policy registry for hot-reloads
    policy_registry: Option<ranvier_core::policy::PolicyRegistry>,
    /// Guard executors registered via `guard()`.
    guard_execs: Vec<Arc<dyn GuardExec>>,
    /// Response extractors from guard registrations.
    guard_response_extractors: Vec<ResponseExtractorFn>,
    /// Response body transforms from guard registrations (e.g., compression).
    guard_body_transforms: Vec<ResponseBodyTransformFn>,
    /// CORS preflight configuration from guards that handle preflight.
    preflight_config: Option<PreflightConfig>,
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
            #[cfg(feature = "tls")]
            tls_config: None,
            #[cfg(feature = "http3")]
            http3_config: None,
            #[cfg(feature = "http3")]
            alt_svc_h3_port: None,
            active_intervention: false,
            policy_registry: None,
            guard_execs: Vec::new(),
            guard_response_extractors: Vec::new(),
            guard_body_transforms: Vec::new(),
            preflight_config: None,
            _phantom: std::marker::PhantomData,
        }
    }

    // ── Server Configuration ─────────────────────────────────────────────

    /// Set the bind address for the server.
    pub fn bind(mut self, addr: impl Into<String>) -> Self {
        self.addr = Some(addr.into());
        self
    }

    // ── Policies & Intervention ───────────────────────────────────────────

    /// Enable active intervention endpoints (`/_system/intervene/*`).
    /// These endpoints allow external tooling (like Ranvier Studio) to pause,
    /// inspect, and forcefully resume or re-route in-flight workflow instances.
    pub fn active_intervention(mut self) -> Self {
        self.active_intervention = true;
        self
    }

    /// Attach a policy registry for hot-reloads.
    pub fn policy_registry(mut self, registry: ranvier_core::policy::PolicyRegistry) -> Self {
        self.policy_registry = Some(registry);
        self
    }

    // ── Lifecycle Hooks ───────────────────────────────────────────────────

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

    /// Apply a `RanvierConfig` to this builder.
    ///
    /// Reads server settings (bind address, shutdown timeout) from the config
    /// and initializes telemetry if an OTLP endpoint is configured.
    /// Logging should be initialized separately via `config.init_logging()`.
    pub fn config(mut self, config: &ranvier_core::config::RanvierConfig) -> Self {
        self.addr = Some(config.bind_addr());
        self.graceful_shutdown_timeout = config.shutdown_timeout();
        config.init_telemetry();
        self
    }

    /// Enable TLS with certificate and key PEM files (requires `tls` feature).
    #[cfg(feature = "tls")]
    pub fn tls(mut self, cert_path: impl Into<String>, key_path: impl Into<String>) -> Self {
        self.tls_config = Some(TlsAcceptorConfig {
            cert_path: cert_path.into(),
            key_path: key_path.into(),
        });
        self
    }

    // ── Middleware Layers ──────────────────────────────────────────────────

    /// Add built-in timeout middleware that returns `408 Request Timeout`
    /// when the inner service call exceeds `timeout`.
    pub fn timeout_layer(mut self, timeout: Duration) -> Self {
        self.layers.push(timeout_middleware(timeout));
        self
    }

    /// Add built-in request-id middleware.
    ///
    /// Ensures `x-request-id` exists on request and response headers.
    pub fn request_id_layer(mut self) -> Self {
        self.layers.push(request_id_middleware());
        self
    }

    /// Register a request-context injector executed before each circuit run.
    ///
    /// Use this to bridge adapter-layer context (request extensions/headers)
    /// into explicit Bus resources consumed by Transitions.
    pub fn bus_injector<F>(mut self, injector: F) -> Self
    where
        F: Fn(&http::request::Parts, &mut Bus) + Send + Sync + 'static,
    {
        self.bus_injectors.push(Arc::new(injector));
        self
    }

    /// Enable htmx header integration.
    ///
    /// Registers a Bus injector that extracts htmx request headers
    /// (`HX-Request`, `HX-Target`, `HX-Trigger`, `HX-Current-URL`, `HX-Boosted`)
    /// and a response extractor that applies `HxResponseHeaders` from the Bus.
    ///
    /// Requires the `htmx` feature flag.
    #[cfg(feature = "htmx")]
    pub fn htmx_support(mut self) -> Self {
        self.bus_injectors
            .push(Arc::new(crate::htmx::inject_htmx_headers));
        self.guard_response_extractors
            .push(Arc::new(crate::htmx::extract_htmx_response_headers));
        self
    }

    /// Register a Guard for HTTP request validation.
    ///
    /// Guards are executed after Bus injection but before the circuit runs.
    /// They can inject data into the Bus (e.g., parsed headers), validate
    /// requests (returning error responses on rejection), and extract
    /// Bus data into response headers.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use ranvier_guard::{CorsGuard, CorsConfig, SecurityHeadersGuard, SecurityPolicy};
    ///
    /// Ranvier::http()
    ///     .guard(CorsGuard::new(CorsConfig::default()))
    ///     .guard(SecurityHeadersGuard::new(SecurityPolicy::default()))
    ///     .get("/api/data", data_circuit)
    ///     .run(())
    ///     .await
    /// ```
    pub fn guard(mut self, guard: impl GuardIntegration) -> Self {
        let registration = guard.register();
        for injector in registration.bus_injectors {
            self.bus_injectors.push(injector);
        }
        self.guard_execs.push(registration.exec);
        if let Some(extractor) = registration.response_extractor {
            self.guard_response_extractors.push(extractor);
        }
        if let Some(transform) = registration.response_body_transform {
            self.guard_body_transforms.push(transform);
        }
        if registration.handles_preflight {
            if let Some(config) = registration.preflight_config {
                self.preflight_config = Some(config);
            }
        }
        self
    }

    /// Configure HTTP/3 QUIC support.
    #[cfg(feature = "http3")]
    pub fn enable_http3(mut self, config: crate::http3::Http3Config) -> Self {
        self.http3_config = Some(config);
        self
    }

    /// Automatically injects the `Alt-Svc` header into responses to signal HTTP/3 availability.
    #[cfg(feature = "http3")]
    pub fn alt_svc_h3(mut self, port: u16) -> Self {
        self.alt_svc_h3_port = Some(port);
        self
    }

    // ── Introspection ────────────────────────────────────────────────────

    /// Export route metadata snapshot for external tooling.
    pub fn route_descriptors(&self) -> Vec<HttpRouteDescriptor> {
        let mut descriptors = self
            .routes
            .iter()
            .map(|entry| {
                let mut desc = HttpRouteDescriptor::new(entry.method.clone(), entry.pattern.raw.clone());
                desc.body_schema = entry.body_schema.clone();
                desc
            })
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

    // ── Static Assets ────────────────────────────────────────────────────

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

    /// Set the default index filename for directory requests.
    ///
    /// When a request path ends with `/` or matches a directory, the server
    /// appends this filename and attempts to serve the result.
    ///
    /// Example: `.directory_index("index.html")` causes `/static/` to serve
    /// `/static/index.html`.
    pub fn directory_index(mut self, filename: impl Into<String>) -> Self {
        self.static_assets.directory_index = Some(filename.into());
        self
    }

    /// Enable immutable cache headers for hashed static filenames.
    ///
    /// Files matching the pattern `name.HASH.ext` (where HASH is 6+ hex chars)
    /// receive `Cache-Control: public, max-age=31536000, immutable`.
    pub fn immutable_cache(mut self) -> Self {
        self.static_assets.immutable_cache = true;
        self
    }

    /// Serve pre-compressed static file variants (`.br`, `.gz`).
    ///
    /// When enabled, the server checks for Brotli (`.br`) and gzip (`.gz`)
    /// variants of requested files, serving them with the appropriate
    /// `Content-Encoding` header. Priority: `.br` > `.gz` > original.
    pub fn serve_precompressed(mut self) -> Self {
        self.static_assets.serve_precompressed = true;
        self
    }

    /// Enable HTTP Range request support for static file serving.
    ///
    /// When enabled, the server responds with `Accept-Ranges: bytes` and
    /// handles `Range: bytes=X-Y` requests with `206 Partial Content`.
    pub fn enable_range_requests(mut self) -> Self {
        self.static_assets.enable_range_requests = true;
        self
    }

    /// Enable gzip response compression for static assets.
    pub fn compression_layer(mut self) -> Self {
        self.static_assets.enable_compression = true;
        self
    }

    // ── WebSocket ─────────────────────────────────────────────────────────

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
        let ws_guard_execs = Arc::new(self.guard_execs.clone());
        let path_for_pattern = path_str.clone();
        let path_for_handler = path_str;

        let route_handler: RouteHandler<R> =
            Arc::new(move |parts: http::request::Parts, res: &R| {
                let ws_handler = ws_handler.clone();
                let bus_injectors = bus_injectors.clone();
                let ws_guard_execs = ws_guard_execs.clone();
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
                        inject_query_params(&parts, &mut bus);
                        for injector in bus_injectors.iter() {
                            injector(&parts, &mut bus);
                        }
                        for guard_exec in ws_guard_execs.iter() {
                            if let Err(rejection) = guard_exec.exec_guard(&mut bus).await {
                                return json_error_response(rejection.status, &rejection.message);
                            }
                        }

                        // Reconstruct a dummy Request for WebSocket extraction
                        let mut req = Request::from_parts(parts, ());
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
                }) as Pin<Box<dyn Future<Output = HttpResponse> + Send>>
            });

        self.routes.push(RouteEntry {
            method: Method::GET,
            pattern: RoutePattern::parse(&path_for_pattern),
            handler: route_handler,
            layers: Arc::new(Vec::new()),
            apply_global_layers: true,
            needs_body: false,
            body_schema: None,
        });

        self
    }

    // ── Health & Readiness ────────────────────────────────────────────────

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

    // ── Routing ──────────────────────────────────────────────────────────

    /// Register a route with GET method.
    pub fn route<Out, E>(self, path: impl Into<String>, circuit: Axon<(), Out, E, R>) -> Self
    where
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
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
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
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
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
        H: Fn(&E) -> HttpResponse + Send + Sync + 'static,
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
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
        H: Fn(&E) -> HttpResponse + Send + Sync + 'static,
    {
        let path_str: String = path.into();
        let circuit = Arc::new(circuit);
        let error_handler = Arc::new(error_handler);
        let route_bus_injectors = Arc::new(self.bus_injectors.clone());
        let route_guard_execs = Arc::new(self.guard_execs.clone());
        let route_response_extractors = Arc::new(self.guard_response_extractors.clone());
        let route_body_transforms = Arc::new(self.guard_body_transforms.clone());
        let path_for_pattern = path_str.clone();
        let path_for_handler = path_str;
        let method_for_pattern = method.clone();
        let method_for_handler = method;

        let handler: RouteHandler<R> = Arc::new(move |parts: http::request::Parts, res: &R| {
            let circuit = circuit.clone();
            let error_handler = error_handler.clone();
            let route_bus_injectors = route_bus_injectors.clone();
            let route_guard_execs = route_guard_execs.clone();
            let route_response_extractors = route_response_extractors.clone();
            let route_body_transforms = route_body_transforms.clone();
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
                    inject_query_params(&parts, &mut bus);
                    for injector in route_bus_injectors.iter() {
                        injector(&parts, &mut bus);
                    }
                    for guard_exec in route_guard_execs.iter() {
                        if let Err(rejection) = guard_exec.exec_guard(&mut bus).await {
                            let mut response = json_error_response(rejection.status, &rejection.message);
                            for extractor in route_response_extractors.iter() {
                                extractor(&bus, response.headers_mut());
                            }
                            return response;
                        }
                    }
                    // Idempotency cache hit → skip circuit
                    if let Some(cached) = bus.read::<ranvier_guard::IdempotencyCachedResponse>() {
                        let body = Bytes::from(cached.body.clone());
                        let mut response = Response::builder()
                            .status(StatusCode::OK)
                            .header("content-type", "application/json")
                            .body(Full::new(body).map_err(|n: Infallible| match n {}).boxed())
                            .unwrap();
                        for extractor in route_response_extractors.iter() {
                            extractor(&bus, response.headers_mut());
                        }
                        return response;
                    }
                    // Timeout enforcement from TimeoutGuard
                    let result = if let Some(td) = bus.read::<ranvier_guard::TimeoutDeadline>() {
                        let remaining = td.remaining();
                        if remaining.is_zero() {
                            let mut response = json_error_response(
                                StatusCode::REQUEST_TIMEOUT,
                                "Request timeout: pipeline deadline exceeded",
                            );
                            for extractor in route_response_extractors.iter() {
                                extractor(&bus, response.headers_mut());
                            }
                            return response;
                        }
                        match tokio::time::timeout(remaining, circuit.execute((), &res, &mut bus)).await {
                            Ok(result) => result,
                            Err(_) => {
                                let mut response = json_error_response(
                                    StatusCode::REQUEST_TIMEOUT,
                                    "Request timeout: pipeline deadline exceeded",
                                );
                                for extractor in route_response_extractors.iter() {
                                    extractor(&bus, response.headers_mut());
                                }
                                return response;
                            }
                        }
                    } else {
                        circuit.execute((), &res, &mut bus).await
                    };
                    let mut response = outcome_to_response_with_error(result, |error| error_handler(error));
                    for extractor in route_response_extractors.iter() {
                        extractor(&bus, response.headers_mut());
                    }
                    if !route_body_transforms.is_empty() {
                        response = apply_body_transforms(response, &bus, &route_body_transforms).await;
                    }
                    response
                }
                .instrument(span)
                .await
            }) as Pin<Box<dyn Future<Output = HttpResponse> + Send>>
        });

        self.routes.push(RouteEntry {
            method: method_for_pattern,
            pattern: RoutePattern::parse(&path_for_pattern),
            handler,
            layers: route_layers,
            apply_global_layers,
            needs_body: false,
            body_schema: None,
        });
        self
    }

    /// Internal: register a typed-body route. The dispatch layer reads the request
    /// body into `BodyBytes` in `Parts::extensions`; this handler deserializes it
    /// as `T` and passes it as the Axon input.
    ///
    /// `T: JsonSchema` enables automatic OpenAPI request body schema generation.
    fn route_method_typed<T, Out, E>(
        mut self,
        method: Method,
        path: impl Into<String>,
        circuit: Axon<T, Out, E, R>,
    ) -> Self
    where
        T: serde::de::DeserializeOwned + Send + Sync + serde::Serialize + schemars::JsonSchema + 'static,
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        let body_schema = serde_json::to_value(schemars::schema_for!(T)).ok();
        let path_str: String = path.into();
        let circuit = Arc::new(circuit);
        let route_bus_injectors = Arc::new(self.bus_injectors.clone());
        let route_guard_execs = Arc::new(self.guard_execs.clone());
        let route_response_extractors = Arc::new(self.guard_response_extractors.clone());
        let route_body_transforms = Arc::new(self.guard_body_transforms.clone());
        let path_for_pattern = path_str.clone();
        let path_for_handler = path_str;
        let method_for_pattern = method.clone();
        let method_for_handler = method;

        let handler: RouteHandler<R> = Arc::new(move |parts: http::request::Parts, res: &R| {
            let circuit = circuit.clone();
            let route_bus_injectors = route_bus_injectors.clone();
            let route_guard_execs = route_guard_execs.clone();
            let route_response_extractors = route_response_extractors.clone();
            let route_body_transforms = route_body_transforms.clone();
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
                    // Extract body bytes from extensions (set by dispatch layer)
                    let body_bytes = parts
                        .extensions
                        .get::<BodyBytes>()
                        .map(|b| b.0.clone())
                        .unwrap_or_default();

                    // Deserialize the body as T
                    let input: T = match serde_json::from_slice(&body_bytes) {
                        Ok(v) => v,
                        Err(e) => {
                            return json_error_response(
                                StatusCode::BAD_REQUEST,
                                &format!("Invalid request body: {}", e),
                            );
                        }
                    };

                    let mut bus = Bus::new();
                    inject_query_params(&parts, &mut bus);
                    for injector in route_bus_injectors.iter() {
                        injector(&parts, &mut bus);
                    }
                    for guard_exec in route_guard_execs.iter() {
                        if let Err(rejection) = guard_exec.exec_guard(&mut bus).await {
                            let mut response = json_error_response(rejection.status, &rejection.message);
                            for extractor in route_response_extractors.iter() {
                                extractor(&bus, response.headers_mut());
                            }
                            return response;
                        }
                    }
                    // Idempotency cache hit → skip circuit
                    if let Some(cached) = bus.read::<ranvier_guard::IdempotencyCachedResponse>() {
                        let body = Bytes::from(cached.body.clone());
                        let mut response = Response::builder()
                            .status(StatusCode::OK)
                            .header("content-type", "application/json")
                            .body(Full::new(body).map_err(|n: Infallible| match n {}).boxed())
                            .unwrap();
                        for extractor in route_response_extractors.iter() {
                            extractor(&bus, response.headers_mut());
                        }
                        return response;
                    }
                    // Timeout enforcement from TimeoutGuard
                    let result = if let Some(td) = bus.read::<ranvier_guard::TimeoutDeadline>() {
                        let remaining = td.remaining();
                        if remaining.is_zero() {
                            let mut response = json_error_response(
                                StatusCode::REQUEST_TIMEOUT,
                                "Request timeout: pipeline deadline exceeded",
                            );
                            for extractor in route_response_extractors.iter() {
                                extractor(&bus, response.headers_mut());
                            }
                            return response;
                        }
                        match tokio::time::timeout(remaining, circuit.execute(input, &res, &mut bus)).await {
                            Ok(result) => result,
                            Err(_) => {
                                let mut response = json_error_response(
                                    StatusCode::REQUEST_TIMEOUT,
                                    "Request timeout: pipeline deadline exceeded",
                                );
                                for extractor in route_response_extractors.iter() {
                                    extractor(&bus, response.headers_mut());
                                }
                                return response;
                            }
                        }
                    } else {
                        circuit.execute(input, &res, &mut bus).await
                    };
                    let mut response = outcome_to_response_with_error(result, |error| {
                        if cfg!(debug_assertions) {
                            (
                                StatusCode::INTERNAL_SERVER_ERROR,
                                format!("Error: {:?}", error),
                            )
                                .into_response()
                        } else {
                            json_error_response(
                                StatusCode::INTERNAL_SERVER_ERROR,
                                "Internal server error",
                            )
                        }
                    });
                    for extractor in route_response_extractors.iter() {
                        extractor(&bus, response.headers_mut());
                    }
                    if !route_body_transforms.is_empty() {
                        response = apply_body_transforms(response, &bus, &route_body_transforms).await;
                    }
                    response
                }
                .instrument(span)
                .await
            }) as Pin<Box<dyn Future<Output = HttpResponse> + Send>>
        });

        self.routes.push(RouteEntry {
            method: method_for_pattern,
            pattern: RoutePattern::parse(&path_for_pattern),
            handler,
            layers: Arc::new(Vec::new()),
            apply_global_layers: true,
            needs_body: true,
            body_schema,
        });
        self
    }

    pub fn get<Out, E>(self, path: impl Into<String>, circuit: Axon<(), Out, E, R>) -> Self
    where
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
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
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
        H: Fn(&E) -> HttpResponse + Send + Sync + 'static,
    {
        self.route_method_with_error(Method::GET, path, circuit, error_handler)
    }

    pub fn post<Out, E>(self, path: impl Into<String>, circuit: Axon<(), Out, E, R>) -> Self
    where
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        self.route_method(Method::POST, path, circuit)
    }

    /// Register a POST route with type-safe JSON body deserialization.
    ///
    /// The request body is automatically deserialized as `T` and passed as
    /// the Axon input. Returns `400 Bad Request` on deserialization failure.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// #[derive(Deserialize, Serialize, Clone)]
    /// struct CreateOrder { product_id: String, quantity: u32 }
    ///
    /// let ingress = HttpIngress::new()
    ///     .post_typed::<CreateOrder, _, _>("/api/orders", order_pipeline());
    /// // order_pipeline() is Axon<CreateOrder, OrderResponse, E, R>
    /// ```
    pub fn post_typed<T, Out, E>(
        self,
        path: impl Into<String>,
        circuit: Axon<T, Out, E, R>,
    ) -> Self
    where
        T: serde::de::DeserializeOwned + Send + Sync + serde::Serialize + schemars::JsonSchema + 'static,
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        self.route_method_typed::<T, Out, E>(Method::POST, path, circuit)
    }

    /// Register a PUT route with type-safe JSON body deserialization.
    ///
    /// See [`post_typed`](Self::post_typed) for details.
    pub fn put_typed<T, Out, E>(
        self,
        path: impl Into<String>,
        circuit: Axon<T, Out, E, R>,
    ) -> Self
    where
        T: serde::de::DeserializeOwned + Send + Sync + serde::Serialize + schemars::JsonSchema + 'static,
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        self.route_method_typed::<T, Out, E>(Method::PUT, path, circuit)
    }

    /// Register a PATCH route with type-safe JSON body deserialization.
    ///
    /// See [`post_typed`](Self::post_typed) for details.
    pub fn patch_typed<T, Out, E>(
        self,
        path: impl Into<String>,
        circuit: Axon<T, Out, E, R>,
    ) -> Self
    where
        T: serde::de::DeserializeOwned + Send + Sync + serde::Serialize + schemars::JsonSchema + 'static,
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        self.route_method_typed::<T, Out, E>(Method::PATCH, path, circuit)
    }

    /// Internal: register a JSON-body route **without** `JsonSchema` requirement.
    ///
    /// Identical to [`route_method_typed`] but omits the `schemars::JsonSchema` bound
    /// and does not generate an OpenAPI request body schema.
    /// Use `post_json`/`put_json`/`patch_json` for public access.
    fn route_method_json<T, Out, E>(
        mut self,
        method: Method,
        path: impl Into<String>,
        circuit: Axon<T, Out, E, R>,
    ) -> Self
    where
        T: serde::de::DeserializeOwned + Send + Sync + serde::Serialize + 'static,
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        let path_str: String = path.into();
        let circuit = Arc::new(circuit);
        let route_bus_injectors = Arc::new(self.bus_injectors.clone());
        let route_guard_execs = Arc::new(self.guard_execs.clone());
        let route_response_extractors = Arc::new(self.guard_response_extractors.clone());
        let route_body_transforms = Arc::new(self.guard_body_transforms.clone());
        let path_for_pattern = path_str.clone();
        let path_for_handler = path_str;
        let method_for_pattern = method.clone();
        let method_for_handler = method;

        let handler: RouteHandler<R> = Arc::new(move |parts: http::request::Parts, res: &R| {
            let circuit = circuit.clone();
            let route_bus_injectors = route_bus_injectors.clone();
            let route_guard_execs = route_guard_execs.clone();
            let route_response_extractors = route_response_extractors.clone();
            let route_body_transforms = route_body_transforms.clone();
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
                    let body_bytes = parts
                        .extensions
                        .get::<BodyBytes>()
                        .map(|b| b.0.clone())
                        .unwrap_or_default();

                    let input: T = match serde_json::from_slice(&body_bytes) {
                        Ok(v) => v,
                        Err(e) => {
                            return json_error_response(
                                StatusCode::BAD_REQUEST,
                                &format!("Invalid request body: {}", e),
                            );
                        }
                    };

                    let mut bus = Bus::new();
                    inject_query_params(&parts, &mut bus);
                    for injector in route_bus_injectors.iter() {
                        injector(&parts, &mut bus);
                    }
                    for guard_exec in route_guard_execs.iter() {
                        if let Err(rejection) = guard_exec.exec_guard(&mut bus).await {
                            let mut response = json_error_response(rejection.status, &rejection.message);
                            for extractor in route_response_extractors.iter() {
                                extractor(&bus, response.headers_mut());
                            }
                            return response;
                        }
                    }
                    if let Some(cached) = bus.read::<ranvier_guard::IdempotencyCachedResponse>() {
                        let body = Bytes::from(cached.body.clone());
                        let mut response = Response::builder()
                            .status(StatusCode::OK)
                            .header("content-type", "application/json")
                            .body(Full::new(body).map_err(|n: Infallible| match n {}).boxed())
                            .unwrap();
                        for extractor in route_response_extractors.iter() {
                            extractor(&bus, response.headers_mut());
                        }
                        return response;
                    }
                    let result = if let Some(td) = bus.read::<ranvier_guard::TimeoutDeadline>() {
                        let remaining = td.remaining();
                        if remaining.is_zero() {
                            let mut response = json_error_response(
                                StatusCode::REQUEST_TIMEOUT,
                                "Request timeout: pipeline deadline exceeded",
                            );
                            for extractor in route_response_extractors.iter() {
                                extractor(&bus, response.headers_mut());
                            }
                            return response;
                        }
                        match tokio::time::timeout(remaining, circuit.execute(input, &res, &mut bus)).await {
                            Ok(result) => result,
                            Err(_) => {
                                let mut response = json_error_response(
                                    StatusCode::REQUEST_TIMEOUT,
                                    "Request timeout: pipeline deadline exceeded",
                                );
                                for extractor in route_response_extractors.iter() {
                                    extractor(&bus, response.headers_mut());
                                }
                                return response;
                            }
                        }
                    } else {
                        circuit.execute(input, &res, &mut bus).await
                    };
                    let mut response = outcome_to_response_with_error(result, |error| {
                        if cfg!(debug_assertions) {
                            (
                                StatusCode::INTERNAL_SERVER_ERROR,
                                format!("Error: {:?}", error),
                            )
                                .into_response()
                        } else {
                            json_error_response(
                                StatusCode::INTERNAL_SERVER_ERROR,
                                "Internal server error",
                            )
                        }
                    });
                    for extractor in route_response_extractors.iter() {
                        extractor(&bus, response.headers_mut());
                    }
                    if !route_body_transforms.is_empty() {
                        response = apply_body_transforms(response, &bus, &route_body_transforms).await;
                    }
                    response
                }
                .instrument(span)
                .await
            }) as Pin<Box<dyn Future<Output = HttpResponse> + Send>>
        });

        self.routes.push(RouteEntry {
            method: method_for_pattern,
            pattern: RoutePattern::parse(&path_for_pattern),
            handler,
            layers: Arc::new(Vec::new()),
            apply_global_layers: true,
            needs_body: true,
            body_schema: None,
        });
        self
    }

    /// Register a POST route with JSON body deserialization — **no `JsonSchema` required**.
    ///
    /// Unlike [`post_typed`](Self::post_typed), this method does not generate an OpenAPI
    /// request body schema. Use this for internal APIs or rapid prototyping where
    /// `schemars::JsonSchema` derive is unnecessary.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // T only needs Deserialize + Serialize — no JsonSchema derive needed
    /// .post_json::<MyRequest, _, _>("/api/internal", pipeline)
    /// ```
    pub fn post_json<T, Out, E>(
        self,
        path: impl Into<String>,
        circuit: Axon<T, Out, E, R>,
    ) -> Self
    where
        T: serde::de::DeserializeOwned + Send + Sync + serde::Serialize + 'static,
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        self.route_method_json::<T, Out, E>(Method::POST, path, circuit)
    }

    /// Register a PUT route with JSON body deserialization — **no `JsonSchema` required**.
    ///
    /// See [`post_json`](Self::post_json) for details.
    pub fn put_json<T, Out, E>(
        self,
        path: impl Into<String>,
        circuit: Axon<T, Out, E, R>,
    ) -> Self
    where
        T: serde::de::DeserializeOwned + Send + Sync + serde::Serialize + 'static,
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        self.route_method_json::<T, Out, E>(Method::PUT, path, circuit)
    }

    /// Register a PATCH route with JSON body deserialization — **no `JsonSchema` required**.
    ///
    /// See [`post_json`](Self::post_json) for details.
    pub fn patch_json<T, Out, E>(
        self,
        path: impl Into<String>,
        circuit: Axon<T, Out, E, R>,
    ) -> Self
    where
        T: serde::de::DeserializeOwned + Send + Sync + serde::Serialize + 'static,
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        self.route_method_json::<T, Out, E>(Method::PATCH, path, circuit)
    }

    pub fn put<Out, E>(self, path: impl Into<String>, circuit: Axon<(), Out, E, R>) -> Self
    where
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        self.route_method(Method::PUT, path, circuit)
    }

    pub fn delete<Out, E>(self, path: impl Into<String>, circuit: Axon<(), Out, E, R>) -> Self
    where
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        self.route_method(Method::DELETE, path, circuit)
    }

    pub fn patch<Out, E>(self, path: impl Into<String>, circuit: Axon<(), Out, E, R>) -> Self
    where
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        self.route_method(Method::PATCH, path, circuit)
    }

    // ── JSON-out routes: Out: Serialize (no IntoResponse required) ──
    //
    // These methods auto-serialize the Outcome<Out, E> as JSON at the route boundary.
    // Transitions return domain types; the HTTP layer handles serialization.
    // See PHILOSOPHY.md §5 "Infrastructure as Boundary".

    /// Internal: register a bodiless route with JSON auto-serialization.
    ///
    /// Unlike `route_method_with_error_and_layers`, the `Out` type does NOT need
    /// `IntoResponse` — only `Serialize`. The framework serializes `Out` as JSON.
    fn route_method_json_out<Out, E>(
        mut self,
        method: Method,
        path: impl Into<String>,
        circuit: Axon<(), Out, E, R>,
    ) -> Self
    where
        Out: Send + Sync + Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        let path_str: String = path.into();
        let circuit = Arc::new(circuit);
        let route_bus_injectors = Arc::new(self.bus_injectors.clone());
        let route_guard_execs = Arc::new(self.guard_execs.clone());
        let route_response_extractors = Arc::new(self.guard_response_extractors.clone());
        let route_body_transforms = Arc::new(self.guard_body_transforms.clone());
        let path_for_pattern = path_str.clone();
        let path_for_handler = path_str;
        let method_for_pattern = method.clone();
        let method_for_handler = method;

        let handler: RouteHandler<R> = Arc::new(move |parts: http::request::Parts, res: &R| {
            let circuit = circuit.clone();
            let route_bus_injectors = route_bus_injectors.clone();
            let route_guard_execs = route_guard_execs.clone();
            let route_response_extractors = route_response_extractors.clone();
            let route_body_transforms = route_body_transforms.clone();
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
                    inject_query_params(&parts, &mut bus);
                    for injector in route_bus_injectors.iter() {
                        injector(&parts, &mut bus);
                    }
                    for guard_exec in route_guard_execs.iter() {
                        if let Err(rejection) = guard_exec.exec_guard(&mut bus).await {
                            let mut response = json_error_response(rejection.status, &rejection.message);
                            for extractor in route_response_extractors.iter() {
                                extractor(&bus, response.headers_mut());
                            }
                            return response;
                        }
                    }
                    if let Some(cached) = bus.read::<ranvier_guard::IdempotencyCachedResponse>() {
                        let body = Bytes::from(cached.body.clone());
                        let mut response = Response::builder()
                            .status(StatusCode::OK)
                            .header("content-type", "application/json")
                            .body(Full::new(body).map_err(|n: Infallible| match n {}).boxed())
                            .unwrap();
                        for extractor in route_response_extractors.iter() {
                            extractor(&bus, response.headers_mut());
                        }
                        return response;
                    }
                    let result = if let Some(td) = bus.read::<ranvier_guard::TimeoutDeadline>() {
                        let remaining = td.remaining();
                        if remaining.is_zero() {
                            let mut response = json_error_response(
                                StatusCode::REQUEST_TIMEOUT,
                                "Request timeout: pipeline deadline exceeded",
                            );
                            for extractor in route_response_extractors.iter() {
                                extractor(&bus, response.headers_mut());
                            }
                            return response;
                        }
                        match tokio::time::timeout(remaining, circuit.execute((), &res, &mut bus)).await {
                            Ok(result) => result,
                            Err(_) => {
                                let mut response = json_error_response(
                                    StatusCode::REQUEST_TIMEOUT,
                                    "Request timeout: pipeline deadline exceeded",
                                );
                                for extractor in route_response_extractors.iter() {
                                    extractor(&bus, response.headers_mut());
                                }
                                return response;
                            }
                        }
                    } else {
                        circuit.execute((), &res, &mut bus).await
                    };
                    let mut response = outcome_to_json_response(result);
                    for extractor in route_response_extractors.iter() {
                        extractor(&bus, response.headers_mut());
                    }
                    if !route_body_transforms.is_empty() {
                        response = apply_body_transforms(response, &bus, &route_body_transforms).await;
                    }
                    response
                }
                .instrument(span)
                .await
            }) as Pin<Box<dyn Future<Output = HttpResponse> + Send>>
        });

        self.routes.push(RouteEntry {
            method: method_for_pattern,
            pattern: RoutePattern::parse(&path_for_pattern),
            handler,
            layers: Arc::new(Vec::new()),
            apply_global_layers: true,
            needs_body: false,
            body_schema: None,
        });
        self
    }

    /// Internal: register a typed-body route with JSON auto-serialization.
    ///
    /// Combines typed body deserialization (like `route_method_typed`) with
    /// JSON output serialization (like `route_method_json_out`).
    fn route_method_typed_json_out<T, Out, E>(
        mut self,
        method: Method,
        path: impl Into<String>,
        circuit: Axon<T, Out, E, R>,
    ) -> Self
    where
        T: serde::de::DeserializeOwned + Send + Sync + Serialize + schemars::JsonSchema + 'static,
        Out: Send + Sync + Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        let body_schema = serde_json::to_value(schemars::schema_for!(T)).ok();
        let path_str: String = path.into();
        let circuit = Arc::new(circuit);
        let route_bus_injectors = Arc::new(self.bus_injectors.clone());
        let route_guard_execs = Arc::new(self.guard_execs.clone());
        let route_response_extractors = Arc::new(self.guard_response_extractors.clone());
        let route_body_transforms = Arc::new(self.guard_body_transforms.clone());
        let path_for_pattern = path_str.clone();
        let path_for_handler = path_str;
        let method_for_pattern = method.clone();
        let method_for_handler = method;

        let handler: RouteHandler<R> = Arc::new(move |parts: http::request::Parts, res: &R| {
            let circuit = circuit.clone();
            let route_bus_injectors = route_bus_injectors.clone();
            let route_guard_execs = route_guard_execs.clone();
            let route_response_extractors = route_response_extractors.clone();
            let route_body_transforms = route_body_transforms.clone();
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
                    let body_bytes = parts
                        .extensions
                        .get::<BodyBytes>()
                        .map(|b| b.0.clone())
                        .unwrap_or_default();

                    let input: T = match serde_json::from_slice(&body_bytes) {
                        Ok(v) => v,
                        Err(e) => {
                            return json_error_response(
                                StatusCode::BAD_REQUEST,
                                &format!("Invalid request body: {}", e),
                            );
                        }
                    };

                    let mut bus = Bus::new();
                    inject_query_params(&parts, &mut bus);
                    for injector in route_bus_injectors.iter() {
                        injector(&parts, &mut bus);
                    }
                    for guard_exec in route_guard_execs.iter() {
                        if let Err(rejection) = guard_exec.exec_guard(&mut bus).await {
                            let mut response = json_error_response(rejection.status, &rejection.message);
                            for extractor in route_response_extractors.iter() {
                                extractor(&bus, response.headers_mut());
                            }
                            return response;
                        }
                    }
                    if let Some(cached) = bus.read::<ranvier_guard::IdempotencyCachedResponse>() {
                        let body = Bytes::from(cached.body.clone());
                        let mut response = Response::builder()
                            .status(StatusCode::OK)
                            .header("content-type", "application/json")
                            .body(Full::new(body).map_err(|n: Infallible| match n {}).boxed())
                            .unwrap();
                        for extractor in route_response_extractors.iter() {
                            extractor(&bus, response.headers_mut());
                        }
                        return response;
                    }
                    let result = if let Some(td) = bus.read::<ranvier_guard::TimeoutDeadline>() {
                        let remaining = td.remaining();
                        if remaining.is_zero() {
                            let mut response = json_error_response(
                                StatusCode::REQUEST_TIMEOUT,
                                "Request timeout: pipeline deadline exceeded",
                            );
                            for extractor in route_response_extractors.iter() {
                                extractor(&bus, response.headers_mut());
                            }
                            return response;
                        }
                        match tokio::time::timeout(remaining, circuit.execute(input, &res, &mut bus)).await {
                            Ok(result) => result,
                            Err(_) => {
                                let mut response = json_error_response(
                                    StatusCode::REQUEST_TIMEOUT,
                                    "Request timeout: pipeline deadline exceeded",
                                );
                                for extractor in route_response_extractors.iter() {
                                    extractor(&bus, response.headers_mut());
                                }
                                return response;
                            }
                        }
                    } else {
                        circuit.execute(input, &res, &mut bus).await
                    };
                    let mut response = outcome_to_json_response(result);
                    for extractor in route_response_extractors.iter() {
                        extractor(&bus, response.headers_mut());
                    }
                    if !route_body_transforms.is_empty() {
                        response = apply_body_transforms(response, &bus, &route_body_transforms).await;
                    }
                    response
                }
                .instrument(span)
                .await
            }) as Pin<Box<dyn Future<Output = HttpResponse> + Send>>
        });

        self.routes.push(RouteEntry {
            method: method_for_pattern,
            pattern: RoutePattern::parse(&path_for_pattern),
            handler,
            layers: Arc::new(Vec::new()),
            apply_global_layers: true,
            needs_body: true,
            body_schema,
        });
        self
    }

    /// GET route with JSON auto-serialization.
    ///
    /// The Transition returns a domain type (`Out: Serialize`); the HTTP boundary
    /// serializes it to JSON. Unlike [`get`](Self::get), `Out` does NOT need
    /// `IntoResponse`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// #[derive(Serialize)]
    /// struct ApiResponse { items: Vec<Item> }
    ///
    /// // Transition returns Outcome<ApiResponse, AppError>
    /// .get_json_out("/api/items", list_items_axon)
    /// // Response: 200 OK, Content-Type: application/json, body: {"items":[...]}
    /// ```
    pub fn get_json_out<Out, E>(self, path: impl Into<String>, circuit: Axon<(), Out, E, R>) -> Self
    where
        Out: Send + Sync + Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        self.route_method_json_out(Method::GET, path, circuit)
    }

    /// DELETE route with JSON auto-serialization.
    ///
    /// See [`get_json_out`](Self::get_json_out) for details.
    pub fn delete_json_out<Out, E>(self, path: impl Into<String>, circuit: Axon<(), Out, E, R>) -> Self
    where
        Out: Send + Sync + Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        self.route_method_json_out(Method::DELETE, path, circuit)
    }

    /// POST route (bodyless) with JSON auto-serialization.
    ///
    /// Registers a POST endpoint that takes no request body and returns a
    /// JSON-serialized response. Useful for action endpoints like
    /// `/api/interfaces/:id/copy` or `/api/cache/clear` where the POST
    /// semantics indicate a state change but no input body is needed.
    ///
    /// See [`get_json_out`](Self::get_json_out) for details.
    pub fn post_json_out<Out, E>(self, path: impl Into<String>, circuit: Axon<(), Out, E, R>) -> Self
    where
        Out: Send + Sync + Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        self.route_method_json_out(Method::POST, path, circuit)
    }

    /// Create a route group with a shared path prefix and guards.
    ///
    /// Routes registered inside the group closure inherit the prefix and all
    /// guards added via [`RouteGroup::guard`]. Group guards are scoped: they
    /// apply only to routes inside the group and do not affect routes
    /// registered after the group.
    ///
    /// # Nesting
    ///
    /// Groups can be nested up to 2 levels deep (group inside a group).
    /// Attempting to nest further will panic.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// Ranvier::http()
    ///     .group("/api", |g| g
    ///         .guard(RbacGuard::authenticated(&jwt_secret))
    ///         .get_json_out("/departments", dept_list)
    ///         .get_json_out("/departments/:id", dept_detail)
    ///         .group("/admin", |a| a
    ///             .guard(RbacGuard::require_role(&jwt_secret, "admin"))
    ///             .post_typed_json_out("/users", user_create)
    ///         )
    ///     )
    /// ```
    pub fn group(self, prefix: &str, f: impl FnOnce(RouteGroup<R>) -> RouteGroup<R>) -> Self {
        let saved_injectors_len = self.bus_injectors.len();
        let saved_execs_len = self.guard_execs.len();
        let saved_extractors_len = self.guard_response_extractors.len();
        let saved_transforms_len = self.guard_body_transforms.len();

        let group = RouteGroup {
            ingress: self,
            prefix: prefix.to_string(),
            depth: 0,
        };
        let group = f(group);
        let mut ingress = group.ingress;

        ingress.bus_injectors.truncate(saved_injectors_len);
        ingress.guard_execs.truncate(saved_execs_len);
        ingress.guard_response_extractors.truncate(saved_extractors_len);
        ingress.guard_body_transforms.truncate(saved_transforms_len);

        ingress
    }

    /// POST with typed JSON body and JSON auto-serialization.
    ///
    /// Combines `post_typed` (typed body with `JsonSchema`) with automatic
    /// JSON output serialization. The Transition returns `Outcome<Out, E>`
    /// where `Out: Serialize`; the framework handles JSON conversion.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// #[derive(Deserialize, Serialize, JsonSchema)]
    /// struct CreateOrder { product_id: String }
    ///
    /// #[derive(Serialize)]
    /// struct OrderResponse { id: Uuid, status: String }
    ///
    /// // Transition: Outcome<OrderResponse, AppError>
    /// .post_typed_json_out::<CreateOrder, _, _>("/api/orders", create_order_axon)
    /// ```
    pub fn post_typed_json_out<T, Out, E>(
        self,
        path: impl Into<String>,
        circuit: Axon<T, Out, E, R>,
    ) -> Self
    where
        T: serde::de::DeserializeOwned + Send + Sync + Serialize + schemars::JsonSchema + 'static,
        Out: Send + Sync + Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        self.route_method_typed_json_out::<T, Out, E>(Method::POST, path, circuit)
    }

    /// PUT with typed JSON body and JSON auto-serialization.
    ///
    /// See [`post_typed_json_out`](Self::post_typed_json_out) for details.
    pub fn put_typed_json_out<T, Out, E>(
        self,
        path: impl Into<String>,
        circuit: Axon<T, Out, E, R>,
    ) -> Self
    where
        T: serde::de::DeserializeOwned + Send + Sync + Serialize + schemars::JsonSchema + 'static,
        Out: Send + Sync + Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        self.route_method_typed_json_out::<T, Out, E>(Method::PUT, path, circuit)
    }

    /// PATCH with typed JSON body and JSON auto-serialization.
    ///
    /// See [`post_typed_json_out`](Self::post_typed_json_out) for details.
    pub fn patch_typed_json_out<T, Out, E>(
        self,
        path: impl Into<String>,
        circuit: Axon<T, Out, E, R>,
    ) -> Self
    where
        T: serde::de::DeserializeOwned + Send + Sync + Serialize + schemars::JsonSchema + 'static,
        Out: Send + Sync + Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        self.route_method_typed_json_out::<T, Out, E>(Method::PATCH, path, circuit)
    }

    pub fn post_with_error<Out, E, H>(
        self,
        path: impl Into<String>,
        circuit: Axon<(), Out, E, R>,
        error_handler: H,
    ) -> Self
    where
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
        H: Fn(&E) -> HttpResponse + Send + Sync + 'static,
    {
        self.route_method_with_error(Method::POST, path, circuit, error_handler)
    }

    pub fn put_with_error<Out, E, H>(
        self,
        path: impl Into<String>,
        circuit: Axon<(), Out, E, R>,
        error_handler: H,
    ) -> Self
    where
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
        H: Fn(&E) -> HttpResponse + Send + Sync + 'static,
    {
        self.route_method_with_error(Method::PUT, path, circuit, error_handler)
    }

    pub fn delete_with_error<Out, E, H>(
        self,
        path: impl Into<String>,
        circuit: Axon<(), Out, E, R>,
        error_handler: H,
    ) -> Self
    where
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
        H: Fn(&E) -> HttpResponse + Send + Sync + 'static,
    {
        self.route_method_with_error(Method::DELETE, path, circuit, error_handler)
    }

    pub fn patch_with_error<Out, E, H>(
        self,
        path: impl Into<String>,
        circuit: Axon<(), Out, E, R>,
        error_handler: H,
    ) -> Self
    where
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
        H: Fn(&E) -> HttpResponse + Send + Sync + 'static,
    {
        self.route_method_with_error(Method::PATCH, path, circuit, error_handler)
    }

    // ── SSE Streaming API ───────────────────────────────────────────────

    /// Register a POST route that executes a `StreamingAxon` and responds
    /// with Server-Sent Events (SSE).
    ///
    /// The Axon prefix runs first (`() → Out`), then the `StreamingTransition`
    /// produces a stream of items. Each item is serialized as JSON and sent
    /// as an SSE `data:` frame. When the stream completes, a `data: [DONE]`
    /// sentinel is sent.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let streaming = Axon::simple::<String>("chat")
    ///     .then(ClassifyIntent)
    ///     .then_stream(SynthesizeStream);
    ///
    /// Ranvier::http()
    ///     .post_sse("/api/chat/stream", streaming)
    ///     .run().await;
    /// ```
    #[cfg(feature = "streaming")]
    pub fn post_sse<Item, E>(
        self,
        path: impl Into<String>,
        circuit: ranvier_runtime::StreamingAxon<(), Item, E, R>,
    ) -> Self
    where
        Item: serde::Serialize + Send + Sync + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        self.route_sse_internal::<(), Item, E>(Method::POST, path, circuit, false)
    }

    /// Like `post_sse`, but with type-safe JSON body deserialization.
    ///
    /// The request body is deserialized as `T` and passed as the
    /// `StreamingAxon` input. Returns `400 Bad Request` on parse failure.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let streaming = Axon::typed::<ChatRequest, String>("chat")
    ///     .then(ClassifyIntent)
    ///     .then_stream(SynthesizeStream);
    ///
    /// Ranvier::http()
    ///     .post_sse_typed::<ChatRequest, _, _>("/api/chat/stream", streaming)
    ///     .run().await;
    /// ```
    #[cfg(feature = "streaming")]
    pub fn post_sse_typed<T, Item, E>(
        self,
        path: impl Into<String>,
        circuit: ranvier_runtime::StreamingAxon<T, Item, E, R>,
    ) -> Self
    where
        T: serde::de::DeserializeOwned + Send + Sync + serde::Serialize + schemars::JsonSchema + 'static,
        Item: serde::Serialize + Send + Sync + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        self.route_sse_internal::<T, Item, E>(Method::POST, path, circuit, true)
    }

    /// Internal: Register an SSE streaming route.
    #[cfg(feature = "streaming")]
    fn route_sse_internal<T, Item, E>(
        mut self,
        method: Method,
        path: impl Into<String>,
        circuit: ranvier_runtime::StreamingAxon<T, Item, E, R>,
        needs_body: bool,
    ) -> Self
    where
        T: serde::de::DeserializeOwned + Send + Sync + serde::Serialize + 'static,
        Item: serde::Serialize + Send + Sync + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        let path_str: String = path.into();
        let circuit = Arc::new(circuit);
        let route_bus_injectors = Arc::new(self.bus_injectors.clone());
        let route_guard_execs = Arc::new(self.guard_execs.clone());
        let route_response_extractors = Arc::new(self.guard_response_extractors.clone());
        let path_for_pattern = path_str.clone();
        let path_for_handler = path_str;
        let method_for_pattern = method.clone();
        let method_for_handler = method;

        let handler: RouteHandler<R> = Arc::new(move |parts: http::request::Parts, res: &R| {
            let circuit = circuit.clone();
            let route_bus_injectors = route_bus_injectors.clone();
            let route_guard_execs = route_guard_execs.clone();
            let route_response_extractors = route_response_extractors.clone();
            let res = res.clone();
            let path = path_for_handler.clone();
            let method = method_for_handler.clone();

            Box::pin(async move {
                let request_id = uuid::Uuid::new_v4().to_string();
                let span = tracing::info_span!(
                    "SSERequest",
                    ranvier.http.method = %method,
                    ranvier.http.path = %path,
                    ranvier.http.request_id = %request_id
                );

                async move {
                    // Parse typed body if needed
                    let input: T = if needs_body {
                        let body_bytes = parts
                            .extensions
                            .get::<BodyBytes>()
                            .map(|b| b.0.clone())
                            .unwrap_or_default();

                        match serde_json::from_slice(&body_bytes) {
                            Ok(v) => v,
                            Err(e) => {
                                return json_error_response(
                                    StatusCode::BAD_REQUEST,
                                    &format!("Invalid request body: {}", e),
                                );
                            }
                        }
                    } else {
                        // For non-typed routes, T must be ()
                        // This is safe because post_sse uses T=()
                        match serde_json::from_str("null") {
                            Ok(v) => v,
                            Err(_) => {
                                return json_error_response(
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    "Internal: failed to construct default input",
                                );
                            }
                        }
                    };

                    let mut bus = Bus::new();
                    inject_query_params(&parts, &mut bus);
                    for injector in route_bus_injectors.iter() {
                        injector(&parts, &mut bus);
                    }
                    for guard_exec in route_guard_execs.iter() {
                        if let Err(rejection) = guard_exec.exec_guard(&mut bus).await {
                            let mut response = json_error_response(rejection.status, &rejection.message);
                            for extractor in route_response_extractors.iter() {
                                extractor(&bus, response.headers_mut());
                            }
                            return response;
                        }
                    }

                    // Execute the streaming pipeline
                    let stream = match circuit.execute(input, &res, &mut bus).await {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::error!("Streaming pipeline error: {}", e);
                            if cfg!(debug_assertions) {
                                return json_error_response(
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    &format!("Streaming error: {}", e),
                                );
                            } else {
                                return json_error_response(
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    "Internal server error",
                                );
                            }
                        }
                    };

                    // Bridge stream → mpsc channel → SSE frames.
                    // This decouples the !Sync stream from the Sync body requirement.
                    let buffer_size = circuit.buffer_size;
                    let (tx, mut rx) = tokio::sync::mpsc::channel::<Bytes>(buffer_size);

                    // Spawn a task to consume the stream and send SSE frames
                    tokio::spawn(async move {
                        let mut pinned = Box::pin(stream);
                        while let Some(item) = futures_util::StreamExt::next(&mut pinned).await {
                            let text = match serde_json::to_string(&item) {
                                Ok(json) => format!("data: {}\n\n", json),
                                Err(e) => {
                                    tracing::error!("SSE item serialization error: {}", e);
                                    let err_text = "event: error\ndata: {\"message\":\"serialization error\",\"code\":\"serialize_error\"}\n\n".to_string();
                                    let _ = tx.send(Bytes::from(err_text)).await;
                                    break;
                                }
                            };
                            if tx.send(Bytes::from(text)).await.is_err() {
                                tracing::info!("SSE client disconnected");
                                break;
                            }
                        }
                        // Send [DONE] sentinel
                        let _ = tx.send(Bytes::from("data: [DONE]\n\n")).await;
                    });

                    // Receive channel → frame stream (this is Sync-safe)
                    let frame_stream = async_stream::stream! {
                        while let Some(bytes) = rx.recv().await {
                            yield Ok::<http_body::Frame<Bytes>, std::convert::Infallible>(
                                http_body::Frame::data(bytes)
                            );
                        }
                    };

                    let body = http_body_util::StreamBody::new(frame_stream);
                    Response::builder()
                        .status(StatusCode::OK)
                        .header(http::header::CONTENT_TYPE, "text/event-stream")
                        .header(http::header::CACHE_CONTROL, "no-cache")
                        .header(http::header::CONNECTION, "keep-alive")
                        .body(http_body_util::BodyExt::boxed(body))
                        .expect("Valid SSE response")
                }
                .instrument(span)
                .await
            }) as Pin<Box<dyn Future<Output = HttpResponse> + Send>>
        });

        self.routes.push(RouteEntry {
            method: method_for_pattern,
            pattern: RoutePattern::parse(&path_for_pattern),
            handler,
            layers: Arc::new(Vec::new()),
            apply_global_layers: true,
            needs_body,
            body_schema: None,
        });
        self
    }

    // ── Per-Route Guard API ─────────────────────────────────────────────

    /// Internal: apply extra `RegisteredGuard`s, register a route, then restore
    /// the global guard state. The route handler captures the combined
    /// (global + per-route) guard state via `Arc::clone` at registration time.
    fn route_method_with_extra_guards<Out, E>(
        mut self,
        method: Method,
        path: impl Into<String>,
        circuit: Axon<(), Out, E, R>,
        extra_guards: Vec<RegisteredGuard>,
    ) -> Self
    where
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        // Save current guard state lengths
        let saved_injectors = self.bus_injectors.len();
        let saved_execs = self.guard_execs.len();
        let saved_extractors = self.guard_response_extractors.len();
        let saved_transforms = self.guard_body_transforms.len();

        // Apply per-route guards
        for registration in extra_guards {
            for injector in registration.bus_injectors {
                self.bus_injectors.push(injector);
            }
            self.guard_execs.push(registration.exec);
            if let Some(extractor) = registration.response_extractor {
                self.guard_response_extractors.push(extractor);
            }
            if let Some(transform) = registration.response_body_transform {
                self.guard_body_transforms.push(transform);
            }
        }

        // Register route (clones current guard state into Arc)
        self = self.route_method(method, path, circuit);

        // Restore global guard state
        self.bus_injectors.truncate(saved_injectors);
        self.guard_execs.truncate(saved_execs);
        self.guard_response_extractors.truncate(saved_extractors);
        self.guard_body_transforms.truncate(saved_transforms);

        self
    }

    /// Register a GET route with additional per-route Guards.
    ///
    /// Per-route Guards are combined with global Guards for this route only.
    /// Use the [`guards!`](crate::guards) macro to build the guard list.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use ranvier_http::guards;
    /// use ranvier_guard::prelude::*;
    ///
    /// Ranvier::http()
    ///     .guard(AccessLogGuard::new())  // global
    ///     .get_with_guards("/api/admin", admin_circuit, guards![
    ///         AuthGuard::bearer(vec!["admin-token".into()]),
    ///     ])
    /// ```
    pub fn get_with_guards<Out, E>(
        self,
        path: impl Into<String>,
        circuit: Axon<(), Out, E, R>,
        extra_guards: Vec<RegisteredGuard>,
    ) -> Self
    where
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        self.route_method_with_extra_guards(Method::GET, path, circuit, extra_guards)
    }

    /// Register a POST route with additional per-route Guards.
    ///
    /// Per-route Guards are combined with global Guards for this route only.
    /// Ideal for applying `ContentTypeGuard` and `IdempotencyGuard` to
    /// write endpoints without affecting read endpoints.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use ranvier_http::guards;
    /// use ranvier_guard::prelude::*;
    ///
    /// Ranvier::http()
    ///     .guard(AccessLogGuard::new())  // global
    ///     .post_with_guards("/api/orders", order_circuit, guards![
    ///         ContentTypeGuard::json(),
    ///         IdempotencyGuard::ttl_5min(),
    ///     ])
    ///     .get("/api/orders", list_circuit)  // no extra guards
    /// ```
    pub fn post_with_guards<Out, E>(
        self,
        path: impl Into<String>,
        circuit: Axon<(), Out, E, R>,
        extra_guards: Vec<RegisteredGuard>,
    ) -> Self
    where
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        self.route_method_with_extra_guards(Method::POST, path, circuit, extra_guards)
    }

    /// Register a PUT route with additional per-route Guards.
    pub fn put_with_guards<Out, E>(
        self,
        path: impl Into<String>,
        circuit: Axon<(), Out, E, R>,
        extra_guards: Vec<RegisteredGuard>,
    ) -> Self
    where
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        self.route_method_with_extra_guards(Method::PUT, path, circuit, extra_guards)
    }

    /// Register a DELETE route with additional per-route Guards.
    pub fn delete_with_guards<Out, E>(
        self,
        path: impl Into<String>,
        circuit: Axon<(), Out, E, R>,
        extra_guards: Vec<RegisteredGuard>,
    ) -> Self
    where
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        self.route_method_with_extra_guards(Method::DELETE, path, circuit, extra_guards)
    }

    /// Register a PATCH route with additional per-route Guards.
    pub fn patch_with_guards<Out, E>(
        self,
        path: impl Into<String>,
        circuit: Axon<(), Out, E, R>,
        extra_guards: Vec<RegisteredGuard>,
    ) -> Self
    where
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        self.route_method_with_extra_guards(Method::PATCH, path, circuit, extra_guards)
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
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        let circuit = Arc::new(circuit);
        let fallback_bus_injectors = Arc::new(self.bus_injectors.clone());
        let fallback_guard_execs = Arc::new(self.guard_execs.clone());
        let fallback_response_extractors = Arc::new(self.guard_response_extractors.clone());
        let fallback_body_transforms = Arc::new(self.guard_body_transforms.clone());

        let handler: RouteHandler<R> = Arc::new(move |parts: http::request::Parts, res: &R| {
            let circuit = circuit.clone();
            let fallback_bus_injectors = fallback_bus_injectors.clone();
            let fallback_guard_execs = fallback_guard_execs.clone();
            let fallback_response_extractors = fallback_response_extractors.clone();
            let fallback_body_transforms = fallback_body_transforms.clone();
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
                    inject_query_params(&parts, &mut bus);
                    for injector in fallback_bus_injectors.iter() {
                        injector(&parts, &mut bus);
                    }
                    for guard_exec in fallback_guard_execs.iter() {
                        if let Err(rejection) = guard_exec.exec_guard(&mut bus).await {
                            let mut response = json_error_response(rejection.status, &rejection.message);
                            for extractor in fallback_response_extractors.iter() {
                                extractor(&bus, response.headers_mut());
                            }
                            return response;
                        }
                    }
                    let result: ranvier_core::Outcome<Out, E> =
                        circuit.execute((), &res, &mut bus).await;

                    let mut response = match result {
                        Outcome::Next(output) => {
                            let mut response = output.into_response();
                            *response.status_mut() = StatusCode::NOT_FOUND;
                            response
                        }
                        _ => Response::builder()
                            .status(StatusCode::NOT_FOUND)
                            .body(
                                Full::new(Bytes::from("Not Found"))
                                    .map_err(|never| match never {})
                                    .boxed(),
                            )
                            .expect("valid HTTP response construction"),
                    };
                    for extractor in fallback_response_extractors.iter() {
                        extractor(&bus, response.headers_mut());
                    }
                    if !fallback_body_transforms.is_empty() {
                        response = apply_body_transforms(response, &bus, &fallback_body_transforms).await;
                    }
                    response
                }
                .instrument(span)
                .await
            }) as Pin<Box<dyn Future<Output = HttpResponse> + Send>>
        });

        self.fallback = Some(handler);
        self
    }

    // ── Execution ────────────────────────────────────────────────────────

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

        let mut raw_routes = self.routes;
        if self.active_intervention {
            let handler: RouteHandler<R> = Arc::new(|_parts, _res| {
                Box::pin(async move {
                    Response::builder()
                        .status(StatusCode::OK)
                        .body(
                            Full::new(Bytes::from("Intervention accepted"))
                                .map_err(|never| match never {} as Infallible)
                                .boxed(),
                        )
                        .expect("valid HTTP response construction")
                }) as Pin<Box<dyn Future<Output = HttpResponse> + Send>>
            });

            raw_routes.push(RouteEntry {
                method: Method::POST,
                pattern: RoutePattern::parse("/_system/intervene/force_resume"),
                handler,
                layers: Arc::new(Vec::new()),
                apply_global_layers: true,
                needs_body: false,
                body_schema: None,
            });
        }

        if let Some(registry) = self.policy_registry.clone() {
            let handler: RouteHandler<R> = Arc::new(move |_parts, _res| {
                let _registry = registry.clone();
                Box::pin(async move {
                    // This is a simplified reload endpoint.
                    // In a real implementation, it would parse JSON from the body.
                    // For now, we provide the infrastructure.
                    Response::builder()
                        .status(StatusCode::OK)
                        .body(
                            Full::new(Bytes::from("Policy registry active"))
                                .map_err(|never| match never {} as Infallible)
                                .boxed(),
                        )
                        .expect("valid HTTP response construction")
                }) as Pin<Box<dyn Future<Output = HttpResponse> + Send>>
            });

            raw_routes.push(RouteEntry {
                method: Method::POST,
                pattern: RoutePattern::parse("/_system/policy/reload"),
                handler,
                layers: Arc::new(Vec::new()),
                apply_global_layers: true,
                needs_body: false,
                body_schema: None,
            });
        }
        let routes = Arc::new(raw_routes);
        let fallback = self.fallback;
        let layers = Arc::new(self.layers);
        let health = Arc::new(self.health);
        let static_assets = Arc::new(self.static_assets);
        let preflight_config = Arc::new(self.preflight_config);
        let on_start = self.on_start;
        let on_shutdown = self.on_shutdown;
        let graceful_shutdown_timeout = self.graceful_shutdown_timeout;
        let resources = Arc::new(resources);

        let listener = TcpListener::bind(addr).await?;

        // Build optional TLS acceptor
        #[cfg(feature = "tls")]
        let tls_acceptor = if let Some(ref tls_cfg) = self.tls_config {
            let acceptor = build_tls_acceptor(&tls_cfg.cert_path, &tls_cfg.key_path)?;
            tracing::info!("Ranvier HTTP Ingress listening on https://{}", addr);
            Some(acceptor)
        } else {
            tracing::info!("Ranvier HTTP Ingress listening on http://{}", addr);
            None
        };
        #[cfg(not(feature = "tls"))]
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

                    let routes = routes.clone();
                    let fallback = fallback.clone();
                    let resources = resources.clone();
                    let layers = layers.clone();
                    let health = health.clone();
                    let static_assets = static_assets.clone();
                    let preflight_config = preflight_config.clone();
                    #[cfg(feature = "http3")]
                    let alt_svc_h3_port = self.alt_svc_h3_port;

                    #[cfg(feature = "tls")]
                    let tls_acceptor = tls_acceptor.clone();

                    connections.spawn(async move {
                        let service = build_http_service(
                            routes,
                            fallback,
                            resources,
                            layers,
                            health,
                            static_assets,
                            preflight_config,
                            #[cfg(feature = "http3")] alt_svc_h3_port,
                        );

                        #[cfg(feature = "tls")]
                        if let Some(acceptor) = tls_acceptor {
                            match acceptor.accept(stream).await {
                                Ok(tls_stream) => {
                                    let io = TokioIo::new(tls_stream);
                                    if let Err(err) = http1::Builder::new()
                                        .serve_connection(io, service)
                                        .with_upgrades()
                                        .await
                                    {
                                        if err.is_incomplete_message() && should_suppress_incomplete_message_error() {
                                            tracing::debug!("TLS connection closed before full request body was received: {:?}", err);
                                        } else if err.is_incomplete_message() {
                                            tracing::warn!("TLS connection closed before full request body was received: {:?}", err);
                                        } else {
                                            tracing::error!("Error serving TLS connection: {:?}", err);
                                        }
                                    }
                                }
                                Err(err) => {
                                    tracing::warn!("TLS handshake failed: {:?}", err);
                                }
                            }
                            return;
                        }

                        let io = TokioIo::new(stream);
                        if let Err(err) = http1::Builder::new()
                            .serve_connection(io, service)
                            .with_upgrades()
                            .await
                        {
                            if err.is_incomplete_message() && should_suppress_incomplete_message_error() {
                                tracing::debug!("Connection closed before full request body was received: {:?}", err);
                            } else if err.is_incomplete_message() {
                                tracing::warn!("Connection closed before full request body was received: {:?}", err);
                            } else {
                                tracing::error!("Error serving connection: {:?}", err);
                            }
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

    /// Convert to a raw Hyper Service for integration with existing infrastructure.
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
    /// // Use raw_service with existing Hyper infrastructure
    /// ```
    pub fn into_raw_service(self, resources: R) -> RawIngressService<R> {
        let routes = Arc::new(self.routes);
        let fallback = self.fallback;
        let layers = Arc::new(self.layers);
        let health = Arc::new(self.health);
        let static_assets = Arc::new(self.static_assets);
        let preflight_config = Arc::new(self.preflight_config);
        let resources = Arc::new(resources);

        RawIngressService {
            routes,
            fallback,
            layers,
            health,
            static_assets,
            preflight_config,
            resources,
        }
    }
}

/// Apply registered body transforms (e.g., gzip compression) to the response.
///
/// Collects the response body into bytes, runs each transform, and rebuilds
/// the response with the transformed body.
async fn apply_body_transforms(
    response: HttpResponse,
    bus: &Bus,
    transforms: &[ResponseBodyTransformFn],
) -> HttpResponse {
    use http_body_util::BodyExt;

    let (parts, body) = response.into_parts();

    // Collect body bytes
    let collected = match body.collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => {
            // If body collection fails, return a 500 response
            return Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(
                    Full::new(Bytes::from("body collection failed"))
                        .map_err(|never| match never {})
                        .boxed(),
                )
                .expect("valid response");
        }
    };

    let mut transformed = collected;
    for transform in transforms {
        transformed = transform(bus, transformed);
    }

    Response::from_parts(
        parts,
        Full::new(transformed)
            .map_err(|never| match never {})
            .boxed(),
    )
}

fn build_http_service<R>(
    routes: Arc<Vec<RouteEntry<R>>>,
    fallback: Option<RouteHandler<R>>,
    resources: Arc<R>,
    layers: Arc<Vec<ServiceLayer>>,
    health: Arc<HealthConfig<R>>,
    static_assets: Arc<StaticAssetsConfig>,
    preflight_config: Arc<Option<PreflightConfig>>,
    #[cfg(feature = "http3")] alt_svc_port: Option<u16>,
) -> BoxHttpService
where
    R: ranvier_core::transition::ResourceRequirement + Clone + Send + Sync + 'static,
{
    BoxService::new(move |req: Request<Incoming>| {
        let routes = routes.clone();
        let fallback = fallback.clone();
        let resources = resources.clone();
        let layers = layers.clone();
        let health = health.clone();
        let static_assets = static_assets.clone();
        let preflight_config = preflight_config.clone();

        async move {
            let mut req = req;
            let method = req.method().clone();
            let path = req.uri().path().to_string();

            if let Some(response) =
                maybe_handle_health_request(&method, &path, &health, resources.clone()).await
            {
                return Ok::<_, Infallible>(response.into_response());
            }

            // Handle CORS preflight (OPTIONS) before route matching
            if method == Method::OPTIONS {
                if let Some(ref config) = *preflight_config {
                    let origin = req
                        .headers()
                        .get("origin")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("");
                    let is_wildcard = config.allowed_origins.iter().any(|o| o == "*");
                    let is_allowed = is_wildcard
                        || config.allowed_origins.iter().any(|o| o == origin);

                    if is_allowed || origin.is_empty() {
                        let allow_origin = if is_wildcard {
                            "*".to_string()
                        } else {
                            origin.to_string()
                        };
                        let mut response = Response::builder()
                            .status(StatusCode::NO_CONTENT)
                            .body(
                                Full::new(Bytes::new())
                                    .map_err(|never| match never {})
                                    .boxed(),
                            )
                            .expect("valid preflight response");
                        let headers = response.headers_mut();
                        if let Ok(v) = allow_origin.parse() {
                            headers.insert("access-control-allow-origin", v);
                        }
                        if let Ok(v) = config.allowed_methods.parse() {
                            headers.insert("access-control-allow-methods", v);
                        }
                        if let Ok(v) = config.allowed_headers.parse() {
                            headers.insert("access-control-allow-headers", v);
                        }
                        if let Ok(v) = config.max_age.parse() {
                            headers.insert("access-control-max-age", v);
                        }
                        if config.allow_credentials {
                            headers.insert(
                                "access-control-allow-credentials",
                                "true".parse().expect("valid header value"),
                            );
                        }
                        return Ok(response);
                    }
                }
            }

            if let Some((entry, params)) = find_matching_route(routes.as_slice(), &method, &path) {
                req.extensions_mut().insert(params);
                let effective_layers = if entry.apply_global_layers {
                    merge_layers(&layers, &entry.layers)
                } else {
                    entry.layers.clone()
                };

                if effective_layers.is_empty() {
                    let (mut parts, body) = req.into_parts();
                    if entry.needs_body {
                        match BodyExt::collect(body).await {
                            Ok(collected) => { parts.extensions.insert(BodyBytes(collected.to_bytes())); }
                            Err(_) => {
                                return Ok(json_error_response(
                                    StatusCode::BAD_REQUEST,
                                    "Failed to read request body",
                                ));
                            }
                        }
                    }
                    #[allow(unused_mut)]
                    let mut res = (entry.handler)(parts, &resources).await;
                    #[cfg(feature = "http3")]
                    if let Some(port) = alt_svc_port {
                        if let Ok(val) =
                            http::HeaderValue::from_str(&format!("h3=\":{}\"; ma=86400", port))
                        {
                            res.headers_mut().insert(http::header::ALT_SVC, val);
                        }
                    }
                    Ok::<_, Infallible>(res)
                } else {
                    let route_service = build_route_service(
                        entry.handler.clone(),
                        resources.clone(),
                        effective_layers,
                        entry.needs_body,
                    );
                    #[allow(unused_mut)]
                    let mut res = route_service.call(req).await;
                    #[cfg(feature = "http3")]
                    #[allow(irrefutable_let_patterns)]
                    if let Ok(ref mut r) = res {
                        if let Some(port) = alt_svc_port {
                            if let Ok(val) =
                                http::HeaderValue::from_str(&format!("h3=\":{}\"; ma=86400", port))
                            {
                                r.headers_mut().insert(http::header::ALT_SVC, val);
                            }
                        }
                    }
                    res
                }
            } else {
                let req =
                    match maybe_handle_static_request(req, &method, &path, static_assets.as_ref())
                        .await
                    {
                        Ok(req) => req,
                        Err(response) => return Ok(response),
                    };

                #[allow(unused_mut)]
                let mut fallback_res = if let Some(ref fb) = fallback {
                    if layers.is_empty() {
                        let (parts, _) = req.into_parts();
                        Ok(fb(parts, &resources).await)
                    } else {
                        let fallback_service =
                            build_route_service(fb.clone(), resources.clone(), layers.clone(), false);
                        fallback_service.call(req).await
                    }
                } else {
                    Ok(Response::builder()
                        .status(StatusCode::NOT_FOUND)
                        .body(
                            Full::new(Bytes::from("Not Found"))
                                .map_err(|never| match never {})
                                .boxed(),
                        )
                        .expect("valid HTTP response construction"))
                };

                #[cfg(feature = "http3")]
                if let Ok(r) = fallback_res.as_mut() {
                    if let Some(port) = alt_svc_port {
                        if let Ok(val) =
                            http::HeaderValue::from_str(&format!("h3=\":{}\"; ma=86400", port))
                        {
                            r.headers_mut().insert(http::header::ALT_SVC, val);
                        }
                    }
                }

                fallback_res
            }
        }
    })
}

fn build_route_service<R>(
    handler: RouteHandler<R>,
    resources: Arc<R>,
    layers: Arc<Vec<ServiceLayer>>,
    needs_body: bool,
) -> BoxHttpService
where
    R: ranvier_core::transition::ResourceRequirement + Clone + Send + Sync + 'static,
{
    let mut service = BoxService::new(move |req: Request<Incoming>| {
        let handler = handler.clone();
        let resources = resources.clone();
        async move {
            let (mut parts, body) = req.into_parts();
            if needs_body {
                match BodyExt::collect(body).await {
                    Ok(collected) => { parts.extensions.insert(BodyBytes(collected.to_bytes())); }
                    Err(_) => {
                        return Ok(json_error_response(
                            StatusCode::BAD_REQUEST,
                            "Failed to read request body",
                        ));
                    }
                }
            }
            Ok::<_, Infallible>(handler(parts, &resources).await)
        }
    });

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
) -> Option<HttpResponse>
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

/// Serve a single file from the filesystem with MIME type detection and ETag.
async fn serve_single_file(file_path: &str) -> Result<Response<Full<Bytes>>, std::io::Error> {
    let path = std::path::Path::new(file_path);
    let content = tokio::fs::read(path).await?;
    let mime = guess_mime(file_path);
    let mut response = Response::new(Full::new(Bytes::from(content)));
    if let Ok(value) = http::HeaderValue::from_str(mime) {
        response
            .headers_mut()
            .insert(http::header::CONTENT_TYPE, value);
    }
    if let Ok(metadata) = tokio::fs::metadata(path).await {
        if let Ok(modified) = metadata.modified() {
            if let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH) {
                let etag = format!("\"{}\"", duration.as_secs());
                if let Ok(value) = http::HeaderValue::from_str(&etag) {
                    response.headers_mut().insert(http::header::ETAG, value);
                }
            }
        }
    }
    Ok(response)
}

/// Serve a file from a static directory with path traversal protection.
async fn serve_static_file(
    directory: &str,
    file_subpath: &str,
    config: &StaticAssetsConfig,
    if_none_match: Option<&http::HeaderValue>,
    accept_encoding: Option<&http::HeaderValue>,
    range_header: Option<&http::HeaderValue>,
) -> Result<Response<Full<Bytes>>, std::io::Error> {
    let subpath = file_subpath.trim_start_matches('/');

    // Directory index: redirect empty or trailing-slash paths to index file
    let resolved_subpath;
    if subpath.is_empty() || subpath.ends_with('/') {
        if let Some(ref index) = config.directory_index {
            resolved_subpath = if subpath.is_empty() {
                index.clone()
            } else {
                format!("{}{}", subpath, index)
            };
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "empty path",
            ));
        }
    } else {
        resolved_subpath = subpath.to_string();
    }

    let full_path = std::path::Path::new(directory).join(&resolved_subpath);
    // Path traversal protection
    let canonical = tokio::fs::canonicalize(&full_path).await?;
    let dir_canonical = tokio::fs::canonicalize(directory).await?;
    if !canonical.starts_with(&dir_canonical) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "path traversal detected",
        ));
    }

    // Compute ETag from modification time
    let etag = if let Ok(metadata) = tokio::fs::metadata(&canonical).await {
        metadata
            .modified()
            .ok()
            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| format!("\"{}\"", d.as_secs()))
    } else {
        None
    };

    // 304 Not Modified: compare If-None-Match with ETag
    if let (Some(client_etag), Some(server_etag)) = (if_none_match, &etag) {
        if client_etag.as_bytes() == server_etag.as_bytes() {
            let mut response = Response::new(Full::new(Bytes::new()));
            *response.status_mut() = StatusCode::NOT_MODIFIED;
            if let Ok(value) = http::HeaderValue::from_str(server_etag) {
                response.headers_mut().insert(http::header::ETAG, value);
            }
            return Ok(response);
        }
    }

    // Pre-compressed file serving: check for .br / .gz variants
    let (serve_path, content_encoding) = if config.serve_precompressed {
        let client_accepts = accept_encoding
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let canonical_str = canonical.to_str().unwrap_or("");

        if client_accepts.contains("br") {
            let br_path = format!("{}.br", canonical_str);
            if tokio::fs::metadata(&br_path).await.is_ok() {
                (std::path::PathBuf::from(br_path), Some("br"))
            } else if client_accepts.contains("gzip") {
                let gz_path = format!("{}.gz", canonical_str);
                if tokio::fs::metadata(&gz_path).await.is_ok() {
                    (std::path::PathBuf::from(gz_path), Some("gzip"))
                } else {
                    (canonical.clone(), None)
                }
            } else {
                (canonical.clone(), None)
            }
        } else if client_accepts.contains("gzip") {
            let gz_path = format!("{}.gz", canonical_str);
            if tokio::fs::metadata(&gz_path).await.is_ok() {
                (std::path::PathBuf::from(gz_path), Some("gzip"))
            } else {
                (canonical.clone(), None)
            }
        } else {
            (canonical.clone(), None)
        }
    } else {
        (canonical.clone(), None)
    };

    let content = tokio::fs::read(&serve_path).await?;
    // MIME type from original path, not compressed variant
    let mime = guess_mime(canonical.to_str().unwrap_or(""));

    // Range request support
    if config.enable_range_requests {
        if let Some(range_val) = range_header {
            if let Some(response) = handle_range_request(
                range_val,
                &content,
                mime,
                etag.as_deref(),
                content_encoding,
            ) {
                return Ok(response);
            }
        }
    }

    let mut response = Response::new(Full::new(Bytes::from(content)));
    if let Ok(value) = http::HeaderValue::from_str(mime) {
        response
            .headers_mut()
            .insert(http::header::CONTENT_TYPE, value);
    }
    if let Some(ref etag_val) = etag {
        if let Ok(value) = http::HeaderValue::from_str(etag_val) {
            response.headers_mut().insert(http::header::ETAG, value);
        }
    }
    if let Some(encoding) = content_encoding {
        if let Ok(value) = http::HeaderValue::from_str(encoding) {
            response
                .headers_mut()
                .insert(http::header::CONTENT_ENCODING, value);
        }
    }
    if config.enable_range_requests {
        response
            .headers_mut()
            .insert(http::header::ACCEPT_RANGES, http::HeaderValue::from_static("bytes"));
    }

    // Immutable cache for hashed filenames (e.g., app.a1b2c3d4.js)
    if config.immutable_cache {
        if let Some(filename) = canonical.file_name().and_then(|n| n.to_str()) {
            if is_hashed_filename(filename) {
                if let Ok(value) = http::HeaderValue::from_str(
                    "public, max-age=31536000, immutable",
                ) {
                    response
                        .headers_mut()
                        .insert(http::header::CACHE_CONTROL, value);
                }
            }
        }
    }

    Ok(response)
}

/// Handle a Range request, returning a 206 Partial Content response.
///
/// Returns `None` if the Range header is malformed or unsatisfiable.
fn handle_range_request(
    range_header: &http::HeaderValue,
    content: &[u8],
    mime: &str,
    etag: Option<&str>,
    content_encoding: Option<&str>,
) -> Option<Response<Full<Bytes>>> {
    let range_str = range_header.to_str().ok()?;
    let range_spec = range_str.strip_prefix("bytes=")?;
    let total = content.len();
    if total == 0 {
        return None;
    }

    let (start, end) = if let Some(suffix) = range_spec.strip_prefix('-') {
        // bytes=-N  (last N bytes)
        let n: usize = suffix.parse().ok()?;
        if n == 0 || n > total {
            return Some(range_not_satisfiable(total));
        }
        (total - n, total - 1)
    } else if range_spec.ends_with('-') {
        // bytes=N-  (from N to end)
        let start: usize = range_spec.trim_end_matches('-').parse().ok()?;
        if start >= total {
            return Some(range_not_satisfiable(total));
        }
        (start, total - 1)
    } else {
        // bytes=N-M
        let mut parts = range_spec.splitn(2, '-');
        let start: usize = parts.next()?.parse().ok()?;
        let end: usize = parts.next()?.parse().ok()?;
        if start > end || start >= total {
            return Some(range_not_satisfiable(total));
        }
        (start, end.min(total - 1))
    };

    let slice = &content[start..=end];
    let content_range = format!("bytes {}-{}/{}", start, end, total);

    let mut response = Response::new(Full::new(Bytes::copy_from_slice(slice)));
    *response.status_mut() = StatusCode::PARTIAL_CONTENT;
    if let Ok(v) = http::HeaderValue::from_str(&content_range) {
        response.headers_mut().insert(http::header::CONTENT_RANGE, v);
    }
    if let Ok(v) = http::HeaderValue::from_str(mime) {
        response
            .headers_mut()
            .insert(http::header::CONTENT_TYPE, v);
    }
    response
        .headers_mut()
        .insert(http::header::ACCEPT_RANGES, http::HeaderValue::from_static("bytes"));
    if let Some(etag_val) = etag {
        if let Ok(v) = http::HeaderValue::from_str(etag_val) {
            response.headers_mut().insert(http::header::ETAG, v);
        }
    }
    if let Some(encoding) = content_encoding {
        if let Ok(v) = http::HeaderValue::from_str(encoding) {
            response
                .headers_mut()
                .insert(http::header::CONTENT_ENCODING, v);
        }
    }
    Some(response)
}

/// Return a 416 Range Not Satisfiable response.
fn range_not_satisfiable(total: usize) -> Response<Full<Bytes>> {
    let content_range = format!("bytes */{}", total);
    let mut response = Response::new(Full::new(Bytes::from("Range Not Satisfiable")));
    *response.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
    if let Ok(v) = http::HeaderValue::from_str(&content_range) {
        response.headers_mut().insert(http::header::CONTENT_RANGE, v);
    }
    response
}

/// Check if a filename contains a content hash (e.g., `app.a1b2c3d4.js`).
/// Matches the pattern: `name.HEXHASH.ext` where HEXHASH is 6+ hex chars.
fn is_hashed_filename(filename: &str) -> bool {
    let parts: Vec<&str> = filename.rsplitn(3, '.').collect();
    if parts.len() < 3 {
        return false;
    }
    // parts[0] = ext, parts[1] = potential hash, parts[2] = name
    let hash_part = parts[1];
    hash_part.len() >= 6 && hash_part.chars().all(|c| c.is_ascii_hexdigit())
}

fn guess_mime(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" | "ts" | "tsx" => "application/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "avif" => "image/avif",
        "webp" => "image/webp",
        "webm" => "video/webm",
        "mp4" => "video/mp4",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "txt" => "text/plain; charset=utf-8",
        "xml" => "application/xml; charset=utf-8",
        "yaml" | "yml" => "application/yaml",
        "wasm" => "application/wasm",
        "pdf" => "application/pdf",
        "map" => "application/json",
        _ => "application/octet-stream",
    }
}

fn apply_cache_control(
    mut response: Response<Full<Bytes>>,
    cache_control: Option<&str>,
) -> Response<Full<Bytes>> {
    if response.status() == StatusCode::OK {
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

async fn maybe_handle_static_request(
    req: Request<Incoming>,
    method: &Method,
    path: &str,
    static_assets: &StaticAssetsConfig,
) -> Result<Request<Incoming>, HttpResponse> {
    if method != Method::GET && method != Method::HEAD {
        return Ok(req);
    }

    if let Some(mount) = static_assets
        .mounts
        .iter()
        .find(|mount| strip_mount_prefix(path, &mount.route_prefix).is_some())
    {
        let accept_encoding = req.headers().get(http::header::ACCEPT_ENCODING).cloned();
        let if_none_match = req.headers().get(http::header::IF_NONE_MATCH).cloned();
        let range_header = req.headers().get(http::header::RANGE).cloned();
        let Some(stripped_path) = strip_mount_prefix(path, &mount.route_prefix) else {
            return Ok(req);
        };
        let response = match serve_static_file(
            &mount.directory,
            &stripped_path,
            static_assets,
            if_none_match.as_ref(),
            accept_encoding.as_ref(),
            range_header.as_ref(),
        )
        .await
        {
            Ok(response) => response,
            Err(_) => {
                return Err(Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(
                        Full::new(Bytes::from("Failed to serve static asset"))
                            .map_err(|never| match never {})
                            .boxed(),
                    )
                    .unwrap_or_else(|_| {
                        Response::new(
                            Full::new(Bytes::new())
                                .map_err(|never| match never {})
                                .boxed(),
                        )
                    }));
            }
        };
        let mut response = apply_cache_control(response, static_assets.cache_control.as_deref());
        response = maybe_compress_static_response(
            response,
            accept_encoding,
            static_assets.enable_compression,
        );
        let (parts, body) = response.into_parts();
        return Err(Response::from_parts(
            parts,
            body.map_err(|never| match never {}).boxed(),
        ));
    }

    if let Some(spa_file) = static_assets.spa_fallback.as_ref() {
        if looks_like_spa_request(path) {
            let accept_encoding = req.headers().get(http::header::ACCEPT_ENCODING).cloned();
            let response = match serve_single_file(spa_file).await {
                Ok(response) => response,
                Err(_) => {
                    return Err(Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .body(
                            Full::new(Bytes::from("Failed to serve SPA fallback"))
                                .map_err(|never| match never {})
                                .boxed(),
                        )
                        .unwrap_or_else(|_| {
                            Response::new(
                                Full::new(Bytes::new())
                                    .map_err(|never| match never {})
                                    .boxed(),
                            )
                        }));
                }
            };
            let mut response =
                apply_cache_control(response, static_assets.cache_control.as_deref());
            response = maybe_compress_static_response(
                response,
                accept_encoding,
                static_assets.enable_compression,
            );
            let (parts, body) = response.into_parts();
            return Err(Response::from_parts(
                parts,
                body.map_err(|never| match never {}).boxed(),
            ));
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

fn looks_like_spa_request(path: &str) -> bool {
    let tail = path.rsplit('/').next().unwrap_or_default();
    !tail.contains('.')
}

fn maybe_compress_static_response(
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

    let accept_str = accept_encoding.to_str().unwrap_or("");
    if !accept_str.contains("gzip") {
        return response;
    }

    let status = response.status();
    let headers = response.headers().clone();
    let body = response.into_body();

    // Full<Bytes> resolves immediately — collect synchronously via now_or_never()
    let data = futures_util::FutureExt::now_or_never(BodyExt::collect(body))
        .and_then(|r| r.ok())
        .map(|collected| collected.to_bytes())
        .unwrap_or_default();

    // Compress with gzip
    let compressed = {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        let _ = encoder.write_all(&data);
        encoder.finish().unwrap_or_default()
    };

    let mut builder = Response::builder().status(status);
    for (name, value) in headers.iter() {
        if name != http::header::CONTENT_LENGTH && name != http::header::CONTENT_ENCODING {
            builder = builder.header(name, value);
        }
    }
    builder
        .header(http::header::CONTENT_ENCODING, "gzip")
        .body(Full::new(Bytes::from(compressed)))
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::new())))
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
) -> HttpResponse {
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
        .body(
            Full::new(Bytes::from(body))
                .map_err(|never| match never {})
                .boxed(),
        )
        .expect("valid HTTP response construction")
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

/// Build a TLS acceptor from PEM certificate and key files.
#[cfg(feature = "tls")]
fn build_tls_acceptor(
    cert_path: &str,
    key_path: &str,
) -> Result<tokio_rustls::TlsAcceptor, Box<dyn std::error::Error + Send + Sync>> {
    use rustls::ServerConfig;
    use rustls_pemfile::{certs, private_key};
    use std::io::BufReader;
    use tokio_rustls::TlsAcceptor;

    let cert_file = std::fs::File::open(cert_path)
        .map_err(|e| format!("Failed to open certificate file '{}': {}", cert_path, e))?;
    let key_file = std::fs::File::open(key_path)
        .map_err(|e| format!("Failed to open key file '{}': {}", key_path, e))?;

    let cert_chain: Vec<_> = certs(&mut BufReader::new(cert_file))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("Failed to parse certificate PEM: {}", e))?;

    let key = private_key(&mut BufReader::new(key_file))
        .map_err(|e| format!("Failed to parse private key PEM: {}", e))?
        .ok_or("No private key found in key file")?;

    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, key)
        .map_err(|e| format!("TLS configuration error: {}", e))?;

    Ok(TlsAcceptor::from(Arc::new(config)))
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
    health: Arc<HealthConfig<R>>,
    static_assets: Arc<StaticAssetsConfig>,
    preflight_config: Arc<Option<PreflightConfig>>,
    resources: Arc<R>,
}

impl<R> hyper::service::Service<Request<Incoming>> for RawIngressService<R>
where
    R: ranvier_core::transition::ResourceRequirement + Clone + Send + Sync + 'static,
{
    type Response = HttpResponse;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&self, req: Request<Incoming>) -> Self::Future {
        let routes = self.routes.clone();
        let fallback = self.fallback.clone();
        let layers = self.layers.clone();
        let health = self.health.clone();
        let static_assets = self.static_assets.clone();
        let preflight_config = self.preflight_config.clone();
        let resources = self.resources.clone();

        Box::pin(async move {
            let service = build_http_service(
                routes,
                fallback,
                resources,
                layers,
                health,
                static_assets,
                preflight_config,
                #[cfg(feature = "http3")]
                None,
            );
            service.call(req).await
        })
    }
}

// ── Route Group ──────────────────────────────────────────────────────

/// Route group builder for organizing routes with shared prefix and guards.
///
/// Created via [`HttpIngress::group`]. Supports all routing methods from
/// `HttpIngress` with automatic path prefixing and guard inheritance.
///
/// # Nesting
///
/// Groups can be nested up to 2 levels deep. Attempting 3+ levels panics.
///
/// # Example
///
/// ```rust,ignore
/// Ranvier::http()
///     .group("/api", |g| g
///         .guard(RbacGuard::authenticated(&jwt_secret))
///         .get_json_out("/departments", Axon::simple::<String>("dept-list").then(ListDepts))
///         .group("/admin", |a| a
///             .guard(RbacGuard::require_role(&jwt_secret, "admin"))
///             .post_typed_json_out("/users", Axon::typed::<CreateUser, String>("user-create").then(CreateUser))
///         )
///     )
/// ```
pub struct RouteGroup<R = ()> {
    ingress: HttpIngress<R>,
    prefix: String,
    depth: usize,
}

impl<R> RouteGroup<R>
where
    R: ranvier_core::transition::ResourceRequirement + Clone + Send + Sync + 'static,
{
    fn prefixed(&self, path: impl Into<String>) -> String {
        let p = path.into();
        format!("{}{}", self.prefix, p)
    }

    /// Add a guard to all routes in this group.
    pub fn guard(mut self, guard: impl GuardIntegration) -> Self {
        let registration = guard.register();
        for injector in registration.bus_injectors {
            self.ingress.bus_injectors.push(injector);
        }
        self.ingress.guard_execs.push(registration.exec);
        if let Some(extractor) = registration.response_extractor {
            self.ingress.guard_response_extractors.push(extractor);
        }
        if let Some(transform) = registration.response_body_transform {
            self.ingress.guard_body_transforms.push(transform);
        }
        if registration.handles_preflight {
            if let Some(config) = registration.preflight_config {
                self.ingress.preflight_config = Some(config);
            }
        }
        self
    }

    /// Create a nested sub-group with an additional prefix.
    ///
    /// Nested group guards are scoped: they apply only to routes inside the
    /// sub-group and are removed when the sub-group closure returns.
    ///
    /// # Panics
    /// Panics if nesting exceeds 2 levels.
    pub fn group(self, prefix: &str, f: impl FnOnce(RouteGroup<R>) -> RouteGroup<R>) -> Self {
        assert!(
            self.depth < 2,
            "Route groups cannot be nested more than 2 levels deep (attempted depth: {})",
            self.depth + 2
        );

        let my_prefix = self.prefix;
        let my_depth = self.depth;
        let ingress = self.ingress;

        let saved_injectors_len = ingress.bus_injectors.len();
        let saved_execs_len = ingress.guard_execs.len();
        let saved_extractors_len = ingress.guard_response_extractors.len();
        let saved_transforms_len = ingress.guard_body_transforms.len();

        let nested = RouteGroup {
            ingress,
            prefix: format!("{}{}", my_prefix, prefix),
            depth: my_depth + 1,
        };
        let nested = f(nested);
        let mut ingress = nested.ingress;

        ingress.bus_injectors.truncate(saved_injectors_len);
        ingress.guard_execs.truncate(saved_execs_len);
        ingress.guard_response_extractors.truncate(saved_extractors_len);
        ingress.guard_body_transforms.truncate(saved_transforms_len);

        RouteGroup {
            ingress,
            prefix: my_prefix,
            depth: my_depth,
        }
    }

    // ── Bodyless routes with JSON auto-serialization ──

    /// GET route with JSON response. Path is prefixed with the group prefix.
    pub fn get_json_out<Out, E>(mut self, path: impl Into<String>, circuit: Axon<(), Out, E, R>) -> Self
    where
        Out: Send + Sync + Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        let full = self.prefixed(path);
        self.ingress = self.ingress.get_json_out(full, circuit);
        self
    }

    /// POST route (bodyless) with JSON response.
    pub fn post_json_out<Out, E>(mut self, path: impl Into<String>, circuit: Axon<(), Out, E, R>) -> Self
    where
        Out: Send + Sync + Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        let full = self.prefixed(path);
        self.ingress = self.ingress.post_json_out(full, circuit);
        self
    }

    /// DELETE route with JSON response.
    pub fn delete_json_out<Out, E>(mut self, path: impl Into<String>, circuit: Axon<(), Out, E, R>) -> Self
    where
        Out: Send + Sync + Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        let full = self.prefixed(path);
        self.ingress = self.ingress.delete_json_out(full, circuit);
        self
    }

    // ── Typed body + JSON auto-serialization ──

    /// POST with typed JSON body and JSON response.
    pub fn post_typed_json_out<T, Out, E>(mut self, path: impl Into<String>, circuit: Axon<T, Out, E, R>) -> Self
    where
        T: serde::de::DeserializeOwned + Send + Sync + Serialize + schemars::JsonSchema + 'static,
        Out: Send + Sync + Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        let full = self.prefixed(path);
        self.ingress = self.ingress.post_typed_json_out(full, circuit);
        self
    }

    /// PUT with typed JSON body and JSON response.
    pub fn put_typed_json_out<T, Out, E>(mut self, path: impl Into<String>, circuit: Axon<T, Out, E, R>) -> Self
    where
        T: serde::de::DeserializeOwned + Send + Sync + Serialize + schemars::JsonSchema + 'static,
        Out: Send + Sync + Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        let full = self.prefixed(path);
        self.ingress = self.ingress.put_typed_json_out(full, circuit);
        self
    }

    /// PATCH with typed JSON body and JSON response.
    pub fn patch_typed_json_out<T, Out, E>(mut self, path: impl Into<String>, circuit: Axon<T, Out, E, R>) -> Self
    where
        T: serde::de::DeserializeOwned + Send + Sync + Serialize + schemars::JsonSchema + 'static,
        Out: Send + Sync + Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        let full = self.prefixed(path);
        self.ingress = self.ingress.patch_typed_json_out(full, circuit);
        self
    }

    // ── Basic routes (IntoResponse) ──

    /// GET route with IntoResponse output.
    pub fn get<Out, E>(mut self, path: impl Into<String>, circuit: Axon<(), Out, E, R>) -> Self
    where
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        let full = self.prefixed(path);
        self.ingress = self.ingress.get(full, circuit);
        self
    }

    /// POST route (bodyless) with IntoResponse output.
    pub fn post<Out, E>(mut self, path: impl Into<String>, circuit: Axon<(), Out, E, R>) -> Self
    where
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        let full = self.prefixed(path);
        self.ingress = self.ingress.post(full, circuit);
        self
    }

    /// PUT route (bodyless) with IntoResponse output.
    pub fn put<Out, E>(mut self, path: impl Into<String>, circuit: Axon<(), Out, E, R>) -> Self
    where
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        let full = self.prefixed(path);
        self.ingress = self.ingress.put(full, circuit);
        self
    }

    /// PATCH route (bodyless) with IntoResponse output.
    pub fn patch<Out, E>(mut self, path: impl Into<String>, circuit: Axon<(), Out, E, R>) -> Self
    where
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        let full = self.prefixed(path);
        self.ingress = self.ingress.patch(full, circuit);
        self
    }

    /// DELETE route with IntoResponse output.
    pub fn delete<Out, E>(mut self, path: impl Into<String>, circuit: Axon<(), Out, E, R>) -> Self
    where
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        let full = self.prefixed(path);
        self.ingress = self.ingress.delete(full, circuit);
        self
    }

    // ── Typed body routes (IntoResponse) ──

    /// POST with typed JSON body and IntoResponse output.
    pub fn post_typed<T, Out, E>(mut self, path: impl Into<String>, circuit: Axon<T, Out, E, R>) -> Self
    where
        T: serde::de::DeserializeOwned + Send + Sync + serde::Serialize + schemars::JsonSchema + 'static,
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        let full = self.prefixed(path);
        self.ingress = self.ingress.post_typed(full, circuit);
        self
    }

    /// PUT with typed JSON body and IntoResponse output.
    pub fn put_typed<T, Out, E>(mut self, path: impl Into<String>, circuit: Axon<T, Out, E, R>) -> Self
    where
        T: serde::de::DeserializeOwned + Send + Sync + serde::Serialize + schemars::JsonSchema + 'static,
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        let full = self.prefixed(path);
        self.ingress = self.ingress.put_typed(full, circuit);
        self
    }

    /// PATCH with typed JSON body and IntoResponse output.
    pub fn patch_typed<T, Out, E>(mut self, path: impl Into<String>, circuit: Axon<T, Out, E, R>) -> Self
    where
        T: serde::de::DeserializeOwned + Send + Sync + serde::Serialize + schemars::JsonSchema + 'static,
        Out: IntoResponse + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    {
        let full = self.prefixed(path);
        self.ingress = self.ingress.patch_typed(full, circuit);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures_util::{SinkExt, StreamExt};
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
    fn route_without_layer_keeps_empty_route_middleware_stack() {
        let ingress =
            HttpIngress::<()>::new().get("/ping", Axon::<(), (), String, ()>::new("Ping"));
        assert_eq!(ingress.routes.len(), 1);
        assert!(ingress.routes[0].layers.is_empty());
        assert!(ingress.routes[0].apply_global_layers);
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
                if let Some(value) = req.headers.get("x-tenant-id").and_then(|v| v.to_str().ok()) {
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

    #[test]
    fn route_descriptors_export_http_and_health_paths() {
        let ingress = HttpIngress::<()>::new()
            .get(
                "/orders/:id",
                Axon::<(), (), String, ()>::new("OrderById"),
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
        impl Transition<(), String> for SlowDrainRoute {
            type Error = String;
            type Resources = ();

            async fn run(
                &self,
                _state: (),
                _resources: &Self::Resources,
                _bus: &mut Bus,
            ) -> Outcome<String, Self::Error> {
                tokio::time::sleep(Duration::from_millis(120)).await;
                Outcome::next("drained-ok".to_string())
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
                Axon::<(), (), String, ()>::new("SlowDrain").then(SlowDrainRoute),
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
        impl Transition<(), String> for SlowRoute {
            type Error = String;
            type Resources = ();

            async fn run(
                &self,
                _state: (),
                _resources: &Self::Resources,
                _bus: &mut Bus,
            ) -> Outcome<String, Self::Error> {
                tokio::time::sleep(Duration::from_millis(80)).await;
                Outcome::next("slow-ok".to_string())
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
                Axon::<(), (), String, ()>::new("Slow").then(SlowRoute),
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

    // ── Range request tests ─────────────────────────────────────────

    fn extract_body(response: Response<Full<Bytes>>) -> Bytes {
        use http_body_util::BodyExt;
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            let collected = response.into_body().collect().await.unwrap();
            collected.to_bytes()
        })
    }

    #[test]
    fn handle_range_bytes_start_end() {
        let content = b"Hello, World!";
        let range = http::HeaderValue::from_static("bytes=0-4");
        let response =
            super::handle_range_request(&range, content, "text/plain", None, None).unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response.headers().get(http::header::CONTENT_RANGE).unwrap(),
            "bytes 0-4/13"
        );
        assert_eq!(extract_body(response), "Hello");
    }

    #[test]
    fn handle_range_suffix() {
        let content = b"Hello, World!";
        let range = http::HeaderValue::from_static("bytes=-6");
        let response =
            super::handle_range_request(&range, content, "text/plain", None, None).unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response.headers().get(http::header::CONTENT_RANGE).unwrap(),
            "bytes 7-12/13"
        );
    }

    #[test]
    fn handle_range_from_offset() {
        let content = b"Hello, World!";
        let range = http::HeaderValue::from_static("bytes=7-");
        let response =
            super::handle_range_request(&range, content, "text/plain", None, None).unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response.headers().get(http::header::CONTENT_RANGE).unwrap(),
            "bytes 7-12/13"
        );
    }

    #[test]
    fn handle_range_out_of_bounds_returns_416() {
        let content = b"Hello";
        let range = http::HeaderValue::from_static("bytes=10-20");
        let response =
            super::handle_range_request(&range, content, "text/plain", None, None).unwrap();
        assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(
            response.headers().get(http::header::CONTENT_RANGE).unwrap(),
            "bytes */5"
        );
    }

    #[test]
    fn handle_range_includes_accept_ranges_header() {
        let content = b"Hello, World!";
        let range = http::HeaderValue::from_static("bytes=0-0");
        let response =
            super::handle_range_request(&range, content, "text/plain", None, None).unwrap();
        assert_eq!(
            response.headers().get(http::header::ACCEPT_RANGES).unwrap(),
            "bytes"
        );
    }

}
