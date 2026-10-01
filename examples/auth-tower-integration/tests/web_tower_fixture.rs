// Exact exported static Tower bearer adapter; no implicit server harness.
#[rustfmt::skip]
mod public_tower {
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
    Ok(ServiceBuilder::new()
        .layer(ValidateRequestHeaderLayer::bearer(configured_bearer))
        .service(service_fn(move |request: Request<String>| {
            let axon = axon.clone();
            async move {
                if request.headers().get_all(http::header::AUTHORIZATION).iter().count() != 1 {
                    return Ok::<_, Infallible>(reply(StatusCode::UNAUTHORIZED, "Unauthorized".into()));
                }
                let mut bus = Bus::new();
                let report = axon.execute(request.into_body(), &(), &mut bus).await;
                if report.cleanup().is_some_and(|cleanup| !cleanup.is_complete()) {
                    return Ok(reply(StatusCode::SERVICE_UNAVAILABLE, "Reconciliation required".into()));
                }
                // This local example accepts only unpersisted execution.
                Ok(match report {
                    ExecutionReport::Unpersisted { result: Outcome::Next(body), .. } => reply(StatusCode::OK, body),
                    ExecutionReport::Unpersisted { result: Outcome::Fault(_), .. } => reply(StatusCode::BAD_REQUEST, "Request rejected".into()),
                    ExecutionReport::Unpersisted { .. } => reply(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".into()),
                    _ => reply(StatusCode::SERVICE_UNAVAILABLE, "Reconciliation required".into()),
                })
            }
        })))
}

#[tokio::test]
async fn actual_public_tower_service_handles_success_fault_and_denial() {
    for invalid in ["", " ", "has space", "line\nfeed", "nonascii-é"] {
        assert!(build_bearer_app(invalid).is_err());
    }
    let mut app = build_bearer_app("fixture-secret").unwrap();
    for (credential, body, expected, expected_body) in [
        (None, "private-input", StatusCode::UNAUTHORIZED, ""),
        (Some("Bearer wrong"), "private-input", StatusCode::UNAUTHORIZED, ""),
        (Some("Bearer fixture-secret"), "owned-input", StatusCode::OK, "owned-input"),
        (Some("Bearer fixture-secret"), " ", StatusCode::BAD_REQUEST, "Request rejected"),
    ] {
        let mut request = Request::new(body.to_owned());
        if let Some(credential) = credential { request.headers_mut().insert(http::header::AUTHORIZATION, credential.parse().unwrap()); }
        std::future::poll_fn(|cx| app.poll_ready(cx)).await.unwrap();
        let response = app.call(request).await.unwrap();
        assert_eq!(response.status(), expected);
        assert_eq!(response.body(), expected_body);
    }
    let mut duplicate = Request::new("private-input".to_owned());
    for _ in 0..2 { duplicate.headers_mut().append(http::header::AUTHORIZATION, http::HeaderValue::from_static("Bearer fixture-secret")); }
    std::future::poll_fn(|cx| app.poll_ready(cx)).await.unwrap();
    let denied = app.call(duplicate).await.unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(denied.body(), "Unauthorized");
}
}
