#![cfg(feature = "validation")]

use http::StatusCode;
use ranvier_core::transition::ResourceRequirement;
use ranvier_core::{Bus, Outcome, Transition};
use ranvier_guard::prelude::{AuthGuard, RequestIdGuard};
use ranvier_http::prelude::*;
use ranvier_runtime::Axon;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use validator::Validate;

#[derive(Clone)]
struct TestResources {
    calls: Arc<AtomicUsize>,
}

impl ResourceRequirement for TestResources {}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, Validate)]
struct CreateUserInput {
    #[validate(length(min = 3, max = 12))]
    #[schemars(length(min = 3, max = 12), regex(pattern = "^[a-z][a-z0-9_]+$"))]
    username: String,
    #[validate(email)]
    #[schemars(email)]
    email: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CreateUserOutput {
    accepted_username: String,
}

#[derive(Clone)]
struct CreateUser;

#[async_trait::async_trait]
impl Transition<CreateUserInput, CreateUserOutput> for CreateUser {
    type Error = String;
    type Resources = TestResources;

    async fn run(
        &self,
        state: CreateUserInput,
        resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<CreateUserOutput, Self::Error> {
        resources.calls.fetch_add(1, Ordering::SeqCst);
        Outcome::next(CreateUserOutput {
            accepted_username: state.username,
        })
    }
}

fn create_user_circuit() -> Axon<CreateUserInput, CreateUserOutput, String, TestResources> {
    Axon::<CreateUserInput, CreateUserInput, String, TestResources>::new("CreateUser")
        .then(CreateUser)
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
struct CompatibilityInput {
    name: String,
}

#[derive(Clone)]
struct CompatibilityRoute;

#[async_trait::async_trait]
impl Transition<CompatibilityInput, CreateUserOutput> for CompatibilityRoute {
    type Error = String;
    type Resources = TestResources;

    async fn run(
        &self,
        state: CompatibilityInput,
        resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<CreateUserOutput, Self::Error> {
        resources.calls.fetch_add(1, Ordering::SeqCst);
        Outcome::next(CreateUserOutput {
            accepted_username: state.name,
        })
    }
}

fn compatibility_circuit() -> Axon<CompatibilityInput, CreateUserOutput, String, TestResources> {
    Axon::<CompatibilityInput, CompatibilityInput, String, TestResources>::new("Compatibility")
        .then(CompatibilityRoute)
}

#[derive(Debug, Deserialize)]
struct ValidationFailure {
    error: String,
    message: String,
    fields: BTreeMap<String, Vec<String>>,
}

fn resources() -> (TestResources, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    (
        TestResources {
            calls: calls.clone(),
        },
        calls,
    )
}

#[tokio::test]
async fn validated_json_out_accepts_valid_payload_and_runs_transition() {
    let (resources, calls) = resources();
    let ingress =
        Ranvier::http::<TestResources>().post_validated_json_out("/users", create_user_circuit());
    let app = TestApp::new(ingress, resources);

    let response = app
        .send(
            TestRequest::post("/users")
                .json(&CreateUserInput {
                    username: "alice_1".to_string(),
                    email: "alice@example.com".to_string(),
                })
                .expect("request json should serialize"),
        )
        .await
        .expect("request should succeed");

    assert_eq!(response.status(), StatusCode::OK);
    let output: CreateUserOutput = response.json().expect("response should be json");
    assert_eq!(output.accepted_username, "alice_1");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn validated_json_out_rejects_dto_validation_errors_before_transition() {
    let (resources, calls) = resources();
    let ingress =
        Ranvier::http::<TestResources>().post_validated_json_out("/users", create_user_circuit());
    let app = TestApp::new(ingress, resources);

    let response = app
        .send(
            TestRequest::post("/users")
                .json(&json!({
                    "username": "ab",
                    "email": "not-an-email"
                }))
                .expect("request json should serialize"),
        )
        .await
        .expect("request should succeed");

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let failure: ValidationFailure = response.json().expect("validation failure json");
    assert_eq!(failure.error, "validation_failed");
    assert_eq!(failure.message, "request validation failed");
    assert!(failure.fields.contains_key("username"));
    assert!(failure.fields.contains_key("email"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn validated_json_out_runs_guards_before_semantic_validation() {
    let (resources, calls) = resources();
    let ingress = Ranvier::http::<TestResources>()
        .guard(AuthGuard::<TestResources>::bearer(vec![
            "valid-token".to_string(),
        ]))
        .post_validated_json_out("/users", create_user_circuit());
    let app = TestApp::new(ingress, resources);

    let response = app
        .send(
            TestRequest::post("/users")
                .json(&json!({
                    "username": "ab",
                    "email": "not-an-email"
                }))
                .expect("request json should serialize"),
        )
        .await
        .expect("request should succeed");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body: serde_json::Value = response.json().expect("guard failure json");
    assert_eq!(body["error"], "Unauthorized: missing Authorization header");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn validated_json_out_applies_guard_response_extractors_on_validation_failure() {
    let (resources, calls) = resources();
    let ingress = Ranvier::http::<TestResources>()
        .guard(RequestIdGuard::<TestResources>::new())
        .guard(AuthGuard::<TestResources>::bearer(vec![
            "valid-token".to_string(),
        ]))
        .post_validated_json_out("/users", create_user_circuit());
    let app = TestApp::new(ingress, resources);

    let response = app
        .send(
            TestRequest::post("/users")
                .header("authorization", "Bearer valid-token")
                .header("x-request-id", "validation-failure-request")
                .json(&json!({
                    "username": "ab",
                    "email": "not-an-email"
                }))
                .expect("request json should serialize"),
        )
        .await
        .expect("request should succeed");

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        response
            .header("x-request-id")
            .expect("request id response header")
            .to_str()
            .expect("request id should be utf8"),
        "validation-failure-request"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn validated_json_out_keeps_invalid_json_as_bad_request() {
    let (resources, calls) = resources();
    let ingress =
        Ranvier::http::<TestResources>().post_validated_json_out("/users", create_user_circuit());
    let app = TestApp::new(ingress, resources);

    let response = app
        .send(
            TestRequest::post("/users")
                .header("content-type", "application/json")
                .text("{not-json"),
        )
        .await
        .expect("request should succeed");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn route_group_forwards_validated_json_out_methods() {
    let (resources, calls) = resources();
    let ingress = Ranvier::http::<TestResources>().group("/api", |group| {
        group.post_validated_json_out("/users", create_user_circuit())
    });
    let app = TestApp::new(ingress, resources);

    let response = app
        .send(
            TestRequest::post("/api/users")
                .json(&CreateUserInput {
                    username: "bob_1".to_string(),
                    email: "bob@example.com".to_string(),
                })
                .expect("request json should serialize"),
        )
        .await
        .expect("request should succeed");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn existing_typed_json_out_remains_schema_only_with_validation_feature_enabled() {
    let (resources, calls) = resources();
    let ingress =
        Ranvier::http::<TestResources>().post_typed_json_out("/compat", compatibility_circuit());
    let app = TestApp::new(ingress, resources);

    let response = app
        .send(
            TestRequest::post("/compat")
                .json(&CompatibilityInput {
                    name: "schema-only".to_string(),
                })
                .expect("request json should serialize"),
        )
        .await
        .expect("request should succeed");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn validated_json_out_route_descriptor_keeps_request_schema_constraints() {
    let ingress =
        Ranvier::http::<TestResources>().post_validated_json_out("/users", create_user_circuit());
    let descriptors = ingress.route_descriptors();
    let descriptor = descriptors
        .iter()
        .find(|descriptor| descriptor.path_pattern() == "/users")
        .expect("validated route descriptor should exist");
    let schema = descriptor
        .body_schema()
        .expect("validated route should expose request body schema");

    let required = schema["required"]
        .as_array()
        .expect("schema required list should be present");
    assert!(required.contains(&json!("username")));
    assert!(required.contains(&json!("email")));

    let username = &schema["properties"]["username"];
    assert_eq!(username["minLength"], json!(3));
    assert_eq!(username["maxLength"], json!(12));
    assert_eq!(username["pattern"], json!("^[a-z][a-z0-9_]+$"));
}
