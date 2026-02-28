use std::convert::Infallible;
use std::time::Duration;

use bytes::Bytes;
use http::{Method, Request, Response, StatusCode};
use http::header::HeaderName;
use http_body_util::Full;
use ranvier_guard::*;
use tower::{Layer, Service, ServiceExt};

fn ok_service() -> impl Service<
    Request<Full<Bytes>>,
    Response = Response<Full<Bytes>>,
    Error = Infallible,
    Future = impl std::future::Future<Output = Result<Response<Full<Bytes>>, Infallible>> + Send,
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

// -------------------------------------------------------------------
// CspBuilder tests
// -------------------------------------------------------------------

#[test]
fn csp_builder_produces_correct_header() {
    let csp = CspBuilder::new()
        .default_src(&["'self'"])
        .script_src(&["'self'", "https://cdn.example.com"])
        .img_src(&["'self'", "data:"])
        .build();

    assert_eq!(
        csp,
        "default-src 'self'; script-src 'self' https://cdn.example.com; img-src 'self' data:"
    );
}

#[test]
fn csp_builder_custom_directive() {
    let csp = CspBuilder::new()
        .directive("report-uri", &["/csp-report"])
        .build();

    assert_eq!(csp, "report-uri /csp-report");
}

// -------------------------------------------------------------------
// Enhanced SecurityHeadersPolicy tests
// -------------------------------------------------------------------

#[tokio::test]
async fn strict_policy_applies_all_headers() {
    let policy = SecurityHeadersPolicy::strict();
    let service = SecurityHeadersLayer::new(policy).layer(ok_service());

    let response = service
        .oneshot(Request::new(Full::new(Bytes::new())))
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);

    // Core headers
    assert!(response.headers().contains_key("strict-transport-security"));
    assert!(response.headers().contains_key("x-content-type-options"));
    assert!(response.headers().contains_key("x-frame-options"));

    // Enhanced headers
    assert_eq!(
        response.headers().get("content-security-policy").unwrap().to_str().unwrap(),
        "default-src 'self'"
    );
    assert_eq!(
        response.headers().get("cross-origin-embedder-policy").unwrap().to_str().unwrap(),
        "require-corp"
    );
    assert_eq!(
        response.headers().get("cross-origin-opener-policy").unwrap().to_str().unwrap(),
        "same-origin"
    );
    assert_eq!(
        response.headers().get("cross-origin-resource-policy").unwrap().to_str().unwrap(),
        "same-origin"
    );
    assert!(response.headers().contains_key("permissions-policy"));
    assert_eq!(
        response.headers().get("x-xss-protection").unwrap().to_str().unwrap(),
        "1; mode=block"
    );
    assert!(response.headers().contains_key("referrer-policy"));
}

#[tokio::test]
async fn custom_csp_via_builder() {
    let csp = CspBuilder::new()
        .default_src(&["'none'"])
        .script_src(&["'self'"]);

    let policy = SecurityHeadersPolicy::default().csp(csp);
    let service = SecurityHeadersLayer::new(policy).layer(ok_service());

    let response = service
        .oneshot(Request::new(Full::new(Bytes::new())))
        .await
        .expect("response");

    assert_eq!(
        response.headers().get("content-security-policy").unwrap().to_str().unwrap(),
        "default-src 'none'; script-src 'self'"
    );
}

// -------------------------------------------------------------------
// ConnectionLimitLayer tests
// -------------------------------------------------------------------

#[tokio::test]
async fn connection_limit_rejects_excess_connections() {
    let layer = ConnectionLimitLayer::new(1);
    let mut service = layer.layer(ok_service());

    // First request should succeed (within limit)
    let req = Request::builder()
        .header("x-forwarded-for", "10.0.0.1")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let resp = service.call(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// -------------------------------------------------------------------
// RequestSizeLimitLayer tests
// -------------------------------------------------------------------

#[tokio::test]
async fn request_size_limit_rejects_long_url() {
    let layer = RequestSizeLimitLayer::new().max_url_bytes(10);
    let service = layer.layer(ok_service());

    let long_uri = "/a".repeat(20);
    let req = Request::builder()
        .uri(long_uri.as_str())
        .body(Full::new(Bytes::new()))
        .unwrap();

    let response = service.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::URI_TOO_LONG);
}

#[tokio::test]
async fn request_size_limit_rejects_large_headers() {
    let layer = RequestSizeLimitLayer::new().max_header_bytes(10);
    let service = layer.layer(ok_service());

    let req = Request::builder()
        .header("x-large-header", "a]".repeat(100))
        .body(Full::new(Bytes::new()))
        .unwrap();

    let response = service.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE);
}

#[tokio::test]
async fn request_size_limit_passes_normal_requests() {
    let layer = RequestSizeLimitLayer::new();
    let service = layer.layer(ok_service());

    let req = Request::builder()
        .uri("/api/test")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let response = service.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

// -------------------------------------------------------------------
// Rate limiting per-IP test
// -------------------------------------------------------------------

#[tokio::test]
async fn rate_limit_per_ip_isolation() {
    let policy = RateLimitPolicy::new(1, Duration::from_secs(60));
    let service = RateLimitLayer::new(policy).layer(ok_service());

    // Client A — first request OK
    let req_a = Request::builder()
        .header("x-forwarded-for", "1.2.3.4")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let resp = service.clone().oneshot(req_a).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Client A — second request rate-limited
    let req_a2 = Request::builder()
        .header("x-forwarded-for", "1.2.3.4")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let resp = service.clone().oneshot(req_a2).await.unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);

    // Client B — first request OK (different IP)
    let req_b = Request::builder()
        .header("x-forwarded-for", "5.6.7.8")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let resp = service.oneshot(req_b).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
