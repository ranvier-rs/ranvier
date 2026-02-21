use std::collections::HashMap;
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http::header::{HeaderName, HeaderValue};
use http::{Method, Request, Response, StatusCode};
use http_body_util::Full;
use tower::{Layer, Service};
use tower_http::cors::CorsLayer;

type BoxFuture<T> = Pin<Box<dyn Future<Output = Result<T, Infallible>> + Send>>;

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
#[derive(Clone, Debug)]
pub struct SecurityHeadersPolicy {
    pub strict_transport_security: HeaderValue,
    pub x_content_type_options: HeaderValue,
    pub x_frame_options: HeaderValue,
}

impl Default for SecurityHeadersPolicy {
    fn default() -> Self {
        Self {
            strict_transport_security: HeaderValue::from_static(
                "max-age=63072000; includeSubDomains; preload",
            ),
            x_content_type_options: HeaderValue::from_static("nosniff"),
            x_frame_options: HeaderValue::from_static("DENY"),
        }
    }
}

impl SecurityHeadersPolicy {
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
    S: Service<Request<B>, Response = Response<Full<Bytes>>, Error = Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    B: Send + 'static,
{
    type Response = Response<Full<Bytes>>;
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
    S: Service<Request<B>, Response = Response<Full<Bytes>>, Error = Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    B: Send + 'static,
{
    type Response = Response<Full<Bytes>>;
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

fn rate_limited_response(limit: u64) -> Response<Full<Bytes>> {
    let payload = serde_json::json!({
        "error": "rate_limit_exceeded",
        "message": "too many requests",
    });

    let mut response = Response::builder()
        .status(StatusCode::TOO_MANY_REQUESTS)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(payload.to_string())))
        .expect("rate-limit response should be infallible");

    add_rate_headers(response.headers_mut(), limit, 0);
    response
}

pub mod prelude {
    pub use crate::{
        CorsGuardLayer, RateLimitLayer, RateLimitPolicy, SecurityHeadersLayer,
        SecurityHeadersPolicy,
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
        Response = Response<Full<Bytes>>,
        Error = Infallible,
        Future = impl Future<Output = Result<Response<Full<Bytes>>, Infallible>> + Send,
    > + Clone {
        tower::service_fn(|_req: Request<Full<Bytes>>| async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(StatusCode::OK)
                    .body(Full::new(Bytes::from_static(b"ok")))
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
