use http::StatusCode;
use ranvier_core::{Bus, Outcome, Transition};
use ranvier_http::prelude::*;
use ranvier_runtime::Axon;
use serde::Deserialize;

#[derive(Clone)]
struct TextTransition(&'static str);

#[async_trait::async_trait]
impl Transition<(), String> for TextTransition {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        _state: (),
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        Outcome::next(self.0.to_string())
    }
}

#[derive(Debug, Deserialize)]
struct HealthPayload {
    status: String,
    probe: String,
    checks: Vec<HealthCheckPayload>,
}

#[derive(Debug, Deserialize)]
struct HealthCheckPayload {
    name: String,
    status: String,
    error: Option<String>,
}

#[tokio::test]
async fn test_app_hello_world_flow() {
    let ingress = Ranvier::http::<()>().get(
        "/",
        Axon::<(), (), String, ()>::new("HelloWorld").then(TextTransition("hello-world")),
    );

    let app = TestApp::new(ingress, ());
    let response = app
        .send(TestRequest::get("/"))
        .await
        .expect("test request should succeed");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.text().expect("utf8"), "hello-world");
}

#[tokio::test]
async fn test_app_routing_path_match_flow() {
    let ingress = Ranvier::http::<()>().get(
        "/orders/:id",
        Axon::<(), (), String, ()>::new("OrderById").then(TextTransition("order-found")),
    );

    let app = TestApp::new(ingress, ());
    let response = app
        .send(TestRequest::get("/orders/42"))
        .await
        .expect("test request should succeed");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.text().expect("utf8"), "order-found");
}

#[tokio::test]
async fn test_app_fallback_flow() {
    let ingress = Ranvier::http::<()>()
        .get(
            "/known",
            Axon::<(), (), String, ()>::new("Known").then(TextTransition("known-route")),
        )
        .fallback(Axon::<(), (), String, ()>::new("Fallback").then(TextTransition("missing")));

    let app = TestApp::new(ingress, ());
    let response = app
        .send(TestRequest::get("/unknown"))
        .await
        .expect("test request should succeed");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(response.text().expect("utf8"), "missing");
}

#[tokio::test]
async fn health_endpoint_returns_200_with_json_status() {
    let ingress = Ranvier::http::<()>()
        .health_endpoint("/health")
        .health_check("db", |_| async { Ok::<(), &'static str>(()) });

    let app = TestApp::new(ingress, ());
    let response = app
        .send(TestRequest::get("/health"))
        .await
        .expect("test request should succeed");

    assert_eq!(response.status(), StatusCode::OK);
    let payload: HealthPayload = response.json().expect("health json parse");
    assert_eq!(payload.status, "ok");
    assert_eq!(payload.probe, "health");
    assert_eq!(payload.checks.len(), 1);
    assert_eq!(payload.checks[0].name, "db");
    assert_eq!(payload.checks[0].status, "ok");
    assert!(payload.checks[0].error.is_none());
}

#[tokio::test]
async fn readiness_liveness_split_supports_custom_checks() {
    let ingress = Ranvier::http::<()>()
        .health_endpoint("/health")
        .readiness_liveness_default()
        .health_check("dependency", |_| async { Err::<(), _>("dependency down") });

    let app = TestApp::new(ingress, ());

    let live = app
        .send(TestRequest::get("/live"))
        .await
        .expect("liveness request should succeed");
    assert_eq!(live.status(), StatusCode::OK);

    let ready = app
        .send(TestRequest::get("/ready"))
        .await
        .expect("readiness request should succeed");
    assert_eq!(ready.status(), StatusCode::SERVICE_UNAVAILABLE);

    let ready_payload: HealthPayload = ready.json().expect("readiness json parse");
    assert_eq!(ready_payload.probe, "readiness");
    assert_eq!(ready_payload.status, "degraded");
    assert_eq!(ready_payload.checks.len(), 1);
    assert_eq!(ready_payload.checks[0].name, "dependency");
    assert_eq!(ready_payload.checks[0].status, "error");
    assert_eq!(
        ready_payload.checks[0].error.as_deref(),
        Some("dependency down")
    );
}
