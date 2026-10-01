// Exact exported hybrid composition, with its complete shared Axon adapter.
#[rustfmt::skip]
mod public_hybrid {
use http::{Request, Response, StatusCode};
use ranvier_core::{Bus, Outcome};
use ranvier_macros::transition;
use ranvier_runtime::{Axon, ExecutionReport};
use std::convert::Infallible;
use tower::{Service, ServiceBuilder, service_fn};
use tower_http::validate_request::ValidateRequestHeaderLayer;

#[transition]
async fn business_logic(input: String) -> Outcome<String, String> {
    if input.trim().is_empty() {
        Outcome::Fault("Empty request".into())
    } else {
        Outcome::Next(input)
    }
}

fn reply(status: StatusCode, body: String) -> Response<String> {
    let mut response = Response::new(body);
    *response.status_mut() = status;
    response.headers_mut().insert(http::header::CACHE_CONTROL, http::HeaderValue::from_static("no-store"));
    response.headers_mut().insert(http::header::CONTENT_TYPE, http::HeaderValue::from_static("text/plain; charset=utf-8"));
    if status == StatusCode::UNAUTHORIZED {
        response.headers_mut().insert(http::header::WWW_AUTHENTICATE, http::HeaderValue::from_static("Bearer"));
    }
    response
}

// Static bearer credential demonstration; not JWT verification or an HTTP server.
// Enable tower's util and tower-http's auth features. Supply the credential at startup.
fn build_bearer_app(configured_bearer: &str) -> Result<
    impl Service<Request<String>, Response = Response<String>, Error = Infallible> + Clone,
    &'static str,
> {
    if configured_bearer.is_empty() || configured_bearer.len() > 4096
        || !configured_bearer.is_ascii()
        || configured_bearer.bytes().any(|byte| byte.is_ascii_whitespace())
        || http::HeaderValue::from_str(&format!("Bearer {configured_bearer}")).is_err()
    {
        return Err("Invalid bearer configuration");
    }
    let axon = Axon::<String, String, String>::new("tower-business").then(business_logic);
    let credential = configured_bearer.to_owned();
    Ok(ServiceBuilder::new()
        .layer(ValidateRequestHeaderLayer::custom(move |request: &mut Request<String>| {
            let mut headers = request.headers().get_all(http::header::AUTHORIZATION).iter();
            let accepted = headers.next().and_then(|header| header.to_str().ok())
                .and_then(|header| header.split_once(' '))
                .is_some_and(|(scheme, token)| scheme.eq_ignore_ascii_case("Bearer") && token == credential);
            if !accepted || headers.next().is_some() {
                Err(reply(StatusCode::UNAUTHORIZED, "Unauthorized".into()))
            } else {
                Ok(())
            }
        }))
        .service(service_fn(move |request: Request<String>| {
            let axon = axon.clone();
            async move {
                let mut bus = Bus::new();
                let report = axon.execute(request.into_body(), &(), &mut bus).await;
                if report.cleanup().is_some_and(|cleanup| !cleanup.is_complete()) {
                    return Ok(reply(StatusCode::SERVICE_UNAVAILABLE, "Reconciliation required".into()));
                }
                // This local example accepts only unpersisted execution.
                Ok::<_, Infallible>(match report {
                    ExecutionReport::Unpersisted { result: Outcome::Next(body), .. } => reply(StatusCode::OK, body),
                    ExecutionReport::Unpersisted { result: Outcome::Fault(_), .. } => reply(StatusCode::BAD_REQUEST, "Request rejected".into()),
                    ExecutionReport::Unpersisted { .. } => reply(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".into()),
                    _ => reply(StatusCode::SERVICE_UNAVAILABLE, "Reconciliation required".into()),
                })
            }
        })))
}

use std::time::Duration;
use tower::limit::RateLimitLayer;
use tower_http::cors::CorsLayer;

// Enable tower limit/util and tower-http cors/auth features.
// This is one service-wide readiness budget, not a per-user quota or a 429 mapper.
fn build_hybrid_app(credential: &str, origin: &str, requests: u64, period: Duration) -> Result<
    impl Service<Request<String>, Response = Response<String>, Error = Infallible>,
    &'static str,
> {
    if requests == 0 || requests > 1000 || period.is_zero() || period > Duration::from_secs(60) {
        return Err("Invalid rate configuration");
    }
    let uri: http::Uri = origin.parse().map_err(|_| "Invalid CORS origin")?;
    if !matches!(uri.scheme_str(), Some("http" | "https")) || uri.authority().is_none()
        || uri.authority().is_some_and(|authority| authority.as_str().contains('@'))
        || uri.path_and_query().is_some_and(|path| path.as_str() != "/")
        || origin.ends_with('/')
    {
        return Err("Invalid CORS origin");
    }
    let origin = http::HeaderValue::from_str(origin).map_err(|_| "Invalid CORS origin")?;
    let business = build_bearer_app(credential)?;
    Ok(ServiceBuilder::new()
        .layer(CorsLayer::new().allow_origin([origin])
            .allow_methods([http::Method::POST])
            .allow_headers([http::header::AUTHORIZATION, http::header::CONTENT_TYPE]))
        .layer(RateLimitLayer::new(requests, period))
        .service(business))
}

#[tokio::test]
async fn actual_hybrid_composition_validates_config_cors_and_backpressure() {
    for origin in ["*", "https://example.test/path", "https://user@example.test", "ftp://example.test"] {
        assert!(build_hybrid_app("secret", origin, 1, Duration::from_secs(1)).is_err());
    }
    assert!(build_hybrid_app("secret", "https://example.test", 0, Duration::from_secs(1)).is_err());
    assert!(build_hybrid_app("secret", "https://example.test", 1, Duration::ZERO).is_err());
    let mut app = build_hybrid_app("secret", "https://example.test", 1, Duration::from_secs(60)).unwrap();
    let mut request = Request::new("owned-body".to_owned());
    request.headers_mut().insert(http::header::AUTHORIZATION, http::HeaderValue::from_static("Bearer secret"));
    request.headers_mut().insert(http::header::ORIGIN, http::HeaderValue::from_static("https://example.test"));
    std::future::poll_fn(|cx| app.poll_ready(cx)).await.unwrap();
    let response = app.call(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.body(), "owned-body");
    assert_eq!(response.headers()[http::header::ACCESS_CONTROL_ALLOW_ORIGIN], "https://example.test");
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    assert!(app.poll_ready(&mut cx).is_pending());
}

#[tokio::test]
async fn hybrid_preflight_and_disallowed_origin_have_explicit_browser_policy() {
    let mut app = build_hybrid_app("secret", "https://example.test", 10, Duration::from_secs(60)).unwrap();
    let mut preflight = Request::new(String::new());
    *preflight.method_mut() = http::Method::OPTIONS;
    preflight.headers_mut().insert(http::header::ORIGIN, http::HeaderValue::from_static("https://example.test"));
    preflight.headers_mut().insert(http::header::ACCESS_CONTROL_REQUEST_METHOD, http::HeaderValue::from_static("POST"));
    preflight.headers_mut().insert(http::header::ACCESS_CONTROL_REQUEST_HEADERS, http::HeaderValue::from_static("authorization,content-type"));
    std::future::poll_fn(|cx| app.poll_ready(cx)).await.unwrap();
    let response = app.call(preflight).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[http::header::ACCESS_CONTROL_ALLOW_ORIGIN], "https://example.test");
    assert_eq!(response.headers()[http::header::ACCESS_CONTROL_ALLOW_METHODS], "POST");
    assert!(!response.headers().contains_key(http::header::ACCESS_CONTROL_ALLOW_CREDENTIALS));
    let mut request = Request::new("owned-body".to_owned());
    request.headers_mut().insert(http::header::ORIGIN, http::HeaderValue::from_static("https://untrusted.test"));
    request.headers_mut().insert(http::header::AUTHORIZATION, http::HeaderValue::from_static("Bearer secret"));
    std::future::poll_fn(|cx| app.poll_ready(cx)).await.unwrap();
    let response = app.call(request).await.unwrap();
    // CORS controls browser read access; it does not authorize domain execution.
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!response.headers().contains_key(http::header::ACCESS_CONTROL_ALLOW_ORIGIN));
}
}
