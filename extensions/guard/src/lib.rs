use std::collections::HashMap;
use std::convert::Infallible;
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http::header::{HeaderName, HeaderValue};
use http::{Method, Request, Response, StatusCode};
use http_body_util::{BodyExt, Full};
use tower::{Layer, Service};
use tower_http::cors::CorsLayer;

type BoxFuture<T> = Pin<Box<dyn Future<Output = Result<T, Infallible>> + Send>>;

pub mod sanitize;

/// CORS convenience wrapper for Ranvier ingress.
#[derive(Clone)]
pub struct CorsGuardLayer {
    inner: CorsLayer,
}

impl CorsGuardLayer {
    /// Allow all origins/methods/headers.
    pub fn permissive() -> Self {
        Self {
            inner: CorsLayer::permissive(),
        }
    }

    /// Allow a fixed origin list with common API methods.
    pub fn origins<I, S>(origins: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let origin_values = origins
            .into_iter()
            .map(|origin| HeaderValue::from_str(origin.as_ref()).expect("valid CORS origin"))
            .collect::<Vec<_>>();

        let inner = CorsLayer::new()
            .allow_origin(origin_values)
            .allow_methods([
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::PATCH,
                Method::DELETE,
                Method::OPTIONS,
            ])
            .allow_headers(tower_http::cors::Any);

        Self { inner }
    }
}

impl<S> Layer<S> for CorsGuardLayer {
    type Service = <CorsLayer as Layer<S>>::Service;

    fn layer(&self, inner: S) -> Self::Service {
        self.inner.layer(inner)
    }
}

/// Default security-header policy.
///
/// Covers HSTS, X-Content-Type-Options, X-Frame-Options, and optional
/// Content-Security-Policy, Cross-Origin-Embedder-Policy, Cross-Origin-Opener-Policy,
/// Cross-Origin-Resource-Policy, and Permissions-Policy.
#[derive(Clone, Debug)]
pub struct SecurityHeadersPolicy {
    pub strict_transport_security: HeaderValue,
    pub x_content_type_options: HeaderValue,
    pub x_frame_options: HeaderValue,
    pub content_security_policy: Option<HeaderValue>,
    pub cross_origin_embedder_policy: Option<HeaderValue>,
    pub cross_origin_opener_policy: Option<HeaderValue>,
    pub cross_origin_resource_policy: Option<HeaderValue>,
    pub permissions_policy: Option<HeaderValue>,
    pub x_xss_protection: Option<HeaderValue>,
    pub referrer_policy: Option<HeaderValue>,
}

impl Default for SecurityHeadersPolicy {
    fn default() -> Self {
        Self {
            strict_transport_security: HeaderValue::from_static(
                "max-age=63072000; includeSubDomains; preload",
            ),
            x_content_type_options: HeaderValue::from_static("nosniff"),
            x_frame_options: HeaderValue::from_static("DENY"),
            content_security_policy: None,
            cross_origin_embedder_policy: None,
            cross_origin_opener_policy: None,
            cross_origin_resource_policy: None,
            permissions_policy: None,
            x_xss_protection: None,
            referrer_policy: None,
        }
    }
}

impl SecurityHeadersPolicy {
    /// Maximum security preset with all optional headers enabled.
    ///
    /// Includes CSP `default-src 'self'`, COEP `require-corp`, COOP `same-origin`,
    /// CORP `same-origin`, Permissions-Policy (restrictive), X-XSS-Protection,
    /// and strict Referrer-Policy.
    pub fn strict() -> Self {
        Self {
            content_security_policy: Some(HeaderValue::from_static("default-src 'self'")),
            cross_origin_embedder_policy: Some(HeaderValue::from_static("require-corp")),
            cross_origin_opener_policy: Some(HeaderValue::from_static("same-origin")),
            cross_origin_resource_policy: Some(HeaderValue::from_static("same-origin")),
            permissions_policy: Some(HeaderValue::from_static(
                "camera=(), microphone=(), geolocation=()",
            )),
            x_xss_protection: Some(HeaderValue::from_static("1; mode=block")),
            referrer_policy: Some(HeaderValue::from_static("strict-origin-when-cross-origin")),
            ..Default::default()
        }
    }

    /// Set Content-Security-Policy from a `CspBuilder`.
    pub fn csp(mut self, builder: CspBuilder) -> Self {
        self.content_security_policy = Some(
            HeaderValue::from_str(&builder.build()).expect("valid CSP header"),
        );
        self
    }

    /// Set the Cross-Origin-Embedder-Policy header.
    pub fn coep(mut self, value: &'static str) -> Self {
        self.cross_origin_embedder_policy = Some(HeaderValue::from_static(value));
        self
    }

    /// Set the Cross-Origin-Opener-Policy header.
    pub fn coop(mut self, value: &'static str) -> Self {
        self.cross_origin_opener_policy = Some(HeaderValue::from_static(value));
        self
    }

    /// Set the Cross-Origin-Resource-Policy header.
    pub fn corp(mut self, value: &'static str) -> Self {
        self.cross_origin_resource_policy = Some(HeaderValue::from_static(value));
        self
    }

    /// Set the Permissions-Policy header.
    pub fn permissions_policy(mut self, value: &str) -> Self {
        self.permissions_policy = Some(
            HeaderValue::from_str(value).expect("valid Permissions-Policy header"),
        );
        self
    }

    /// Set the Referrer-Policy header.
    pub fn referrer_policy(mut self, value: &'static str) -> Self {
        self.referrer_policy = Some(HeaderValue::from_static(value));
        self
    }

    fn apply(&self, headers: &mut http::HeaderMap) {
        headers.insert(
            HeaderName::from_static("strict-transport-security"),
            self.strict_transport_security.clone(),
        );
        headers.insert(
            HeaderName::from_static("x-content-type-options"),
            self.x_content_type_options.clone(),
        );
        headers.insert(
            HeaderName::from_static("x-frame-options"),
            self.x_frame_options.clone(),
        );
        if let Some(ref csp) = self.content_security_policy {
            headers.insert(
                HeaderName::from_static("content-security-policy"),
                csp.clone(),
            );
        }
        if let Some(ref coep) = self.cross_origin_embedder_policy {
            headers.insert(
                HeaderName::from_static("cross-origin-embedder-policy"),
                coep.clone(),
            );
        }
        if let Some(ref coop) = self.cross_origin_opener_policy {
            headers.insert(
                HeaderName::from_static("cross-origin-opener-policy"),
                coop.clone(),
            );
        }
        if let Some(ref corp) = self.cross_origin_resource_policy {
            headers.insert(
                HeaderName::from_static("cross-origin-resource-policy"),
                corp.clone(),
            );
        }
        if let Some(ref pp) = self.permissions_policy {
            headers.insert(
                HeaderName::from_static("permissions-policy"),
                pp.clone(),
            );
        }
        if let Some(ref xss) = self.x_xss_protection {
            headers.insert(
                HeaderName::from_static("x-xss-protection"),
                xss.clone(),
            );
        }
        if let Some(ref rp) = self.referrer_policy {
            headers.insert(
                HeaderName::from_static("referrer-policy"),
                rp.clone(),
            );
        }
    }
}

/// Fluent builder for Content-Security-Policy header values.
///
/// # Example
/// ```
/// use ranvier_guard::CspBuilder;
///
/// let csp = CspBuilder::new()
///     .default_src(&["'self'"])
///     .script_src(&["'self'", "https://cdn.example.com"])
///     .style_src(&["'self'", "'unsafe-inline'"])
///     .img_src(&["'self'", "data:"])
///     .connect_src(&["'self'", "https://api.example.com"])
///     .frame_ancestors(&["'none'"])
///     .build();
/// ```
#[derive(Clone, Debug, Default)]
pub struct CspBuilder {
    directives: Vec<(String, Vec<String>)>,
}

impl CspBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// `default-src` directive — fallback for all resource types.
    pub fn default_src(self, sources: &[&str]) -> Self {
        self.directive("default-src", sources)
    }

    /// `script-src` directive — controls script sources.
    pub fn script_src(self, sources: &[&str]) -> Self {
        self.directive("script-src", sources)
    }

    /// `style-src` directive — controls stylesheet sources.
    pub fn style_src(self, sources: &[&str]) -> Self {
        self.directive("style-src", sources)
    }

    /// `img-src` directive — controls image sources.
    pub fn img_src(self, sources: &[&str]) -> Self {
        self.directive("img-src", sources)
    }

    /// `font-src` directive — controls font sources.
    pub fn font_src(self, sources: &[&str]) -> Self {
        self.directive("font-src", sources)
    }

    /// `connect-src` directive — controls fetch/XHR/WebSocket destinations.
    pub fn connect_src(self, sources: &[&str]) -> Self {
        self.directive("connect-src", sources)
    }

    /// `media-src` directive — controls audio/video sources.
    pub fn media_src(self, sources: &[&str]) -> Self {
        self.directive("media-src", sources)
    }

    /// `object-src` directive — controls plugin sources.
    pub fn object_src(self, sources: &[&str]) -> Self {
        self.directive("object-src", sources)
    }

    /// `frame-src` directive — controls iframe sources.
    pub fn frame_src(self, sources: &[&str]) -> Self {
        self.directive("frame-src", sources)
    }

    /// `frame-ancestors` directive — controls who can embed this page.
    pub fn frame_ancestors(self, sources: &[&str]) -> Self {
        self.directive("frame-ancestors", sources)
    }

    /// `base-uri` directive — restricts `<base>` element URLs.
    pub fn base_uri(self, sources: &[&str]) -> Self {
        self.directive("base-uri", sources)
    }

    /// `form-action` directive — restricts form submission targets.
    pub fn form_action(self, sources: &[&str]) -> Self {
        self.directive("form-action", sources)
    }

    /// `worker-src` directive — controls Worker/SharedWorker/ServiceWorker sources.
    pub fn worker_src(self, sources: &[&str]) -> Self {
        self.directive("worker-src", sources)
    }

    /// Add a custom CSP directive.
    pub fn directive(mut self, name: &str, sources: &[&str]) -> Self {
        self.directives.push((
            name.to_string(),
            sources.iter().map(|s| s.to_string()).collect(),
        ));
        self
    }

    /// Build the CSP header string value.
    pub fn build(&self) -> String {
        self.directives
            .iter()
            .map(|(name, sources)| format!("{} {}", name, sources.join(" ")))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// Tower layer that inserts security headers into all responses.
#[derive(Clone, Default)]
pub struct SecurityHeadersLayer {
    policy: SecurityHeadersPolicy,
}

impl SecurityHeadersLayer {
    pub fn new(policy: SecurityHeadersPolicy) -> Self {
        Self { policy }
    }
}

impl<S> Layer<S> for SecurityHeadersLayer {
    type Service = SecurityHeadersService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        SecurityHeadersService {
            inner,
            policy: self.policy.clone(),
        }
    }
}

#[derive(Clone)]
pub struct SecurityHeadersService<S> {
    inner: S,
    policy: SecurityHeadersPolicy,
}

impl<S, B> Service<Request<B>> for SecurityHeadersService<S>
where
    S: Service<Request<B>, Response = Response<http_body_util::combinators::BoxBody<bytes::Bytes, std::convert::Infallible>>, Error = Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    B: Send + 'static,
{
    type Response = Response<http_body_util::combinators::BoxBody<bytes::Bytes, std::convert::Infallible>>;
    type Error = Infallible;
    type Future = BoxFuture<Self::Response>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        let mut inner = self.inner.clone();
        let policy = self.policy.clone();

        Box::pin(async move {
            let mut response = inner.call(req).await?;
            policy.apply(response.headers_mut());
            Ok(response)
        })
    }
}

/// In-memory rate-limit policy.
#[derive(Clone, Debug)]
pub struct RateLimitPolicy {
    pub limit: u64,
    pub window: Duration,
    pub key_header: Option<HeaderName>,
}

impl RateLimitPolicy {
    pub fn new(limit: u64, window: Duration) -> Self {
        Self {
            limit: limit.max(1),
            window,
            key_header: Some(HeaderName::from_static("x-forwarded-for")),
        }
    }

    pub fn per_minute(limit: u64) -> Self {
        Self::new(limit, Duration::from_secs(60))
    }

    pub fn key_header(mut self, header: HeaderName) -> Self {
        self.key_header = Some(header);
        self
    }

    pub fn without_key_header(mut self) -> Self {
        self.key_header = None;
        self
    }
}

#[derive(Clone, Debug)]
struct WindowCounter {
    started_at: Instant,
    count: u64,
}

#[derive(Clone)]
pub struct RateLimitLayer {
    policy: RateLimitPolicy,
    state: Arc<Mutex<HashMap<String, WindowCounter>>>,
}

impl RateLimitLayer {
    pub fn new(policy: RateLimitPolicy) -> Self {
        Self {
            policy,
            state: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn per_minute(limit: u64) -> Self {
        Self::new(RateLimitPolicy::per_minute(limit))
    }
}

impl<S> Layer<S> for RateLimitLayer {
    type Service = RateLimitService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RateLimitService {
            inner,
            policy: self.policy.clone(),
            state: self.state.clone(),
        }
    }
}

#[derive(Clone)]
pub struct RateLimitService<S> {
    inner: S,
    policy: RateLimitPolicy,
    state: Arc<Mutex<HashMap<String, WindowCounter>>>,
}

#[derive(Clone, Copy, Debug)]
struct RateDecision {
    allowed: bool,
    remaining: u64,
}

impl<S, B> Service<Request<B>> for RateLimitService<S>
where
    S: Service<Request<B>, Response = Response<http_body_util::combinators::BoxBody<bytes::Bytes, std::convert::Infallible>>, Error = Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    B: Send + 'static,
{
    type Response = Response<http_body_util::combinators::BoxBody<bytes::Bytes, std::convert::Infallible>>;
    type Error = Infallible;
    type Future = BoxFuture<Self::Response>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        let mut inner = self.inner.clone();
        let policy = self.policy.clone();
        let state = self.state.clone();

        Box::pin(async move {
            let key = client_key(&req, &policy);
            let decision = consume_quota(&state, &key, &policy);

            if !decision.allowed {
                return Ok(rate_limited_response(policy.limit));
            }

            let mut response = inner.call(req).await?;
            add_rate_headers(response.headers_mut(), policy.limit, decision.remaining);
            Ok(response)
        })
    }
}

fn client_key<B>(req: &Request<B>, policy: &RateLimitPolicy) -> String {
    if let Some(header) = &policy.key_header {
        if let Some(value) = req.headers().get(header) {
            if let Ok(text) = value.to_str() {
                return text.to_string();
            }
        }
    }
    "global".to_string()
}

fn consume_quota(
    state: &Arc<Mutex<HashMap<String, WindowCounter>>>,
    key: &str,
    policy: &RateLimitPolicy,
) -> RateDecision {
    let now = Instant::now();
    let mut store = state.lock().expect("rate-limit state lock poisoned");

    let entry = store.entry(key.to_string()).or_insert(WindowCounter {
        started_at: now,
        count: 0,
    });

    if now.duration_since(entry.started_at) >= policy.window {
        entry.started_at = now;
        entry.count = 0;
    }

    if entry.count >= policy.limit {
        return RateDecision {
            allowed: false,
            remaining: 0,
        };
    }

    entry.count += 1;
    RateDecision {
        allowed: true,
        remaining: policy.limit.saturating_sub(entry.count),
    }
}

fn add_rate_headers(headers: &mut http::HeaderMap, limit: u64, remaining: u64) {
    headers.insert(
        HeaderName::from_static("x-ratelimit-limit"),
        HeaderValue::from_str(&limit.to_string()).expect("valid rate limit header"),
    );
    headers.insert(
        HeaderName::from_static("x-ratelimit-remaining"),
        HeaderValue::from_str(&remaining.to_string()).expect("valid rate remaining header"),
    );
}

fn rate_limited_response(limit: u64) -> Response<http_body_util::combinators::BoxBody<bytes::Bytes, std::convert::Infallible>> {
    let payload = serde_json::json!({
        "error": "rate_limit_exceeded",
        "message": "too many requests",
    });

    let mut response = Response::builder()
        .status(StatusCode::TOO_MANY_REQUESTS)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(payload.to_string())).boxed())
        .expect("rate-limit response should be infallible");

    add_rate_headers(response.headers_mut(), limit, 0);
    response
}

// ---------------------------------------------------------------------------
// Connection Limit Layer
// ---------------------------------------------------------------------------

/// Tower layer that limits concurrent connections per IP address.
///
/// Prevents connection exhaustion attacks by enforcing a maximum number
/// of simultaneous active requests from a single IP.
#[derive(Clone)]
pub struct ConnectionLimitLayer {
    max_per_ip: usize,
    state: Arc<Mutex<HashMap<IpAddr, Arc<AtomicUsize>>>>,
}

impl ConnectionLimitLayer {
    /// Create a layer that allows at most `max_per_ip` concurrent requests per IP.
    pub fn new(max_per_ip: usize) -> Self {
        Self {
            max_per_ip,
            state: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl<S> Layer<S> for ConnectionLimitLayer {
    type Service = ConnectionLimitService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ConnectionLimitService {
            inner,
            max_per_ip: self.max_per_ip,
            state: self.state.clone(),
        }
    }
}

#[derive(Clone)]
pub struct ConnectionLimitService<S> {
    inner: S,
    max_per_ip: usize,
    state: Arc<Mutex<HashMap<IpAddr, Arc<AtomicUsize>>>>,
}

impl<S, B> Service<Request<B>> for ConnectionLimitService<S>
where
    S: Service<Request<B>, Response = Response<http_body_util::combinators::BoxBody<bytes::Bytes, std::convert::Infallible>>, Error = Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    B: Send + 'static,
{
    type Response = Response<http_body_util::combinators::BoxBody<bytes::Bytes, std::convert::Infallible>>;
    type Error = Infallible;
    type Future = BoxFuture<Self::Response>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        let mut inner = self.inner.clone();
        let max = self.max_per_ip;
        let state = self.state.clone();

        Box::pin(async move {
            let ip = extract_client_ip(&req);
            let counter = {
                let mut store = state.lock().expect("connection-limit state lock");
                store.entry(ip).or_insert_with(|| Arc::new(AtomicUsize::new(0))).clone()
            };

            let current = counter.fetch_add(1, Ordering::SeqCst);
            if current >= max {
                counter.fetch_sub(1, Ordering::SeqCst);
                let payload = serde_json::json!({
                    "error": "connection_limit_exceeded",
                    "message": "too many concurrent connections",
                });
                return Ok(Response::builder()
                    .status(StatusCode::SERVICE_UNAVAILABLE)
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Full::new(Bytes::from(payload.to_string())).boxed())
                    .expect("connection-limit response"));
            }

            let response = inner.call(req).await;
            counter.fetch_sub(1, Ordering::SeqCst);
            response
        })
    }
}

fn extract_client_ip<B>(req: &Request<B>) -> IpAddr {
    // Try X-Forwarded-For first, then X-Real-IP, fall back to loopback
    if let Some(xff) = req.headers().get("x-forwarded-for") {
        if let Ok(text) = xff.to_str() {
            if let Some(first) = text.split(',').next() {
                if let Ok(ip) = first.trim().parse::<IpAddr>() {
                    return ip;
                }
            }
        }
    }
    if let Some(xri) = req.headers().get("x-real-ip") {
        if let Ok(text) = xri.to_str() {
            if let Ok(ip) = text.trim().parse::<IpAddr>() {
                return ip;
            }
        }
    }
    IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
}

// ---------------------------------------------------------------------------
// Request Size Limit Layer
// ---------------------------------------------------------------------------

/// Tower layer that rejects requests exceeding size limits.
///
/// Enforces maximum header sizes and URL lengths to prevent abuse.
#[derive(Clone)]
pub struct RequestSizeLimitLayer {
    /// Maximum total header size in bytes (default: 8KB).
    pub max_header_bytes: usize,
    /// Maximum URL length in bytes (default: 2KB).
    pub max_url_bytes: usize,
}

impl Default for RequestSizeLimitLayer {
    fn default() -> Self {
        Self {
            max_header_bytes: 8 * 1024,
            max_url_bytes: 2 * 1024,
        }
    }
}

impl RequestSizeLimitLayer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set maximum total header size in bytes.
    pub fn max_header_bytes(mut self, max: usize) -> Self {
        self.max_header_bytes = max;
        self
    }

    /// Set maximum URL length in bytes.
    pub fn max_url_bytes(mut self, max: usize) -> Self {
        self.max_url_bytes = max;
        self
    }
}

impl<S> Layer<S> for RequestSizeLimitLayer {
    type Service = RequestSizeLimitService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RequestSizeLimitService {
            inner,
            max_header_bytes: self.max_header_bytes,
            max_url_bytes: self.max_url_bytes,
        }
    }
}

#[derive(Clone)]
pub struct RequestSizeLimitService<S> {
    inner: S,
    max_header_bytes: usize,
    max_url_bytes: usize,
}

impl<S, B> Service<Request<B>> for RequestSizeLimitService<S>
where
    S: Service<Request<B>, Response = Response<http_body_util::combinators::BoxBody<bytes::Bytes, std::convert::Infallible>>, Error = Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    B: Send + 'static,
{
    type Response = Response<http_body_util::combinators::BoxBody<bytes::Bytes, std::convert::Infallible>>;
    type Error = Infallible;
    type Future = BoxFuture<Self::Response>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        let mut inner = self.inner.clone();
        let max_header = self.max_header_bytes;
        let max_url = self.max_url_bytes;

        Box::pin(async move {
            // Check URL length
            let url_len = req.uri().to_string().len();
            if url_len > max_url {
                let payload = serde_json::json!({
                    "error": "url_too_long",
                    "message": format!("URL length {} exceeds limit {}", url_len, max_url),
                });
                return Ok(Response::builder()
                    .status(StatusCode::URI_TOO_LONG)
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Full::new(Bytes::from(payload.to_string())).boxed())
                    .expect("url-limit response"));
            }

            // Check total header size
            let header_bytes: usize = req.headers().iter()
                .map(|(k, v)| k.as_str().len() + v.len())
                .sum();
            if header_bytes > max_header {
                let payload = serde_json::json!({
                    "error": "headers_too_large",
                    "message": format!("header size {} exceeds limit {}", header_bytes, max_header),
                });
                return Ok(Response::builder()
                    .status(StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE)
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Full::new(Bytes::from(payload.to_string())).boxed())
                    .expect("header-limit response"));
            }

            inner.call(req).await
        })
    }
}

pub mod prelude {
    pub use crate::{
        ConnectionLimitLayer, CorsGuardLayer, CspBuilder, RateLimitLayer, RateLimitPolicy,
        RequestSizeLimitLayer, SecurityHeadersLayer, SecurityHeadersPolicy,
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::header::{ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_REQUEST_METHOD, ORIGIN};
    use http_body_util::Full;
    use tower::ServiceExt;

    fn ok_service() -> impl Service<
        Request<Full<Bytes>>,
        Response = Response<http_body_util::combinators::BoxBody<bytes::Bytes, std::convert::Infallible>>,
        Error = Infallible,
        Future = impl Future<Output = Result<Response<http_body_util::combinators::BoxBody<bytes::Bytes, std::convert::Infallible>>, Infallible>> + Send,
    > + Clone {
        tower::service_fn(|_req: Request<Full<Bytes>>| async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(StatusCode::OK)
                    .body(Full::new(Bytes::from_static(b"ok")).boxed())
                    .expect("response build"),
            )
        })
    }

    #[tokio::test]
    async fn cors_permissive_handles_preflight_request() {
        let service = CorsGuardLayer::permissive().layer(ok_service());

        let request = Request::builder()
            .method(Method::OPTIONS)
            .header(ORIGIN, "https://example.com")
            .header(ACCESS_CONTROL_REQUEST_METHOD, "GET")
            .body(Full::new(Bytes::new()))
            .expect("request build");

        let response = service.oneshot(request).await.expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().contains_key(ACCESS_CONTROL_ALLOW_ORIGIN));
    }

    #[tokio::test]
    async fn security_headers_layer_applies_default_headers() {
        let service = SecurityHeadersLayer::default().layer(ok_service());

        let response = service
            .oneshot(Request::new(Full::new(Bytes::new())))
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("strict-transport-security")
                .and_then(|value| value.to_str().ok()),
            Some("max-age=63072000; includeSubDomains; preload")
        );
        assert_eq!(
            response
                .headers()
                .get("x-content-type-options")
                .and_then(|value| value.to_str().ok()),
            Some("nosniff")
        );
        assert_eq!(
            response
                .headers()
                .get("x-frame-options")
                .and_then(|value| value.to_str().ok()),
            Some("DENY")
        );
    }

    #[tokio::test]
    async fn rate_limit_layer_rejects_requests_over_limit() {
        let policy = RateLimitPolicy::new(2, Duration::from_secs(60))
            .key_header(HeaderName::from_static("x-client-id"));
        let service = RateLimitLayer::new(policy).layer(ok_service());

        let first = service
            .clone()
            .oneshot(
                Request::builder()
                    .header("x-client-id", "alpha")
                    .body(Full::new(Bytes::new()))
                    .expect("request build"),
            )
            .await
            .expect("response");
        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(
            first
                .headers()
                .get("x-ratelimit-remaining")
                .and_then(|value| value.to_str().ok()),
            Some("1")
        );

        let second = service
            .clone()
            .oneshot(
                Request::builder()
                    .header("x-client-id", "alpha")
                    .body(Full::new(Bytes::new()))
                    .expect("request build"),
            )
            .await
            .expect("response");
        assert_eq!(second.status(), StatusCode::OK);
        assert_eq!(
            second
                .headers()
                .get("x-ratelimit-remaining")
                .and_then(|value| value.to_str().ok()),
            Some("0")
        );

        let third = service
            .oneshot(
                Request::builder()
                    .header("x-client-id", "alpha")
                    .body(Full::new(Bytes::new()))
                    .expect("request build"),
            )
            .await
            .expect("response");
        assert_eq!(third.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            third
                .headers()
                .get("x-ratelimit-remaining")
                .and_then(|value| value.to_str().ok()),
            Some("0")
        );
    }

    #[tokio::test]
    async fn route_specific_rate_limit_policies_can_differ() {
        let strict = RateLimitLayer::new(
            RateLimitPolicy::new(1, Duration::from_secs(60)).without_key_header(),
        )
        .layer(ok_service());

        let relaxed = RateLimitLayer::new(
            RateLimitPolicy::new(3, Duration::from_secs(60)).without_key_header(),
        )
        .layer(ok_service());

        let strict_first = strict
            .clone()
            .oneshot(Request::new(Full::new(Bytes::new())))
            .await
            .expect("response");
        let strict_second = strict
            .oneshot(Request::new(Full::new(Bytes::new())))
            .await
            .expect("response");
        assert_eq!(strict_first.status(), StatusCode::OK);
        assert_eq!(strict_second.status(), StatusCode::TOO_MANY_REQUESTS);

        let relaxed_first = relaxed
            .clone()
            .oneshot(Request::new(Full::new(Bytes::new())))
            .await
            .expect("response");
        let relaxed_second = relaxed
            .oneshot(Request::new(Full::new(Bytes::new())))
            .await
            .expect("response");
        assert_eq!(relaxed_first.status(), StatusCode::OK);
        assert_eq!(relaxed_second.status(), StatusCode::OK);
    }
}
