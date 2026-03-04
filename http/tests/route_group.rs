// Disabled: RouteGroup API removed in v0.15. Tests preserved for future re-implementation.
#![cfg(feature = "_route_group_tests")]
//! M151 Router DSL Pack — RouteGroup integration tests
//!
//! Validates prefix application, route nesting, empty sub-path semantics,
//! parametric sub-paths, and multi-level group nesting with TestApp.

use std::convert::Infallible;

use http::StatusCode;
use ranvier_core::{Bus, Outcome, Transition};
use ranvier_http::prelude::*;
use ranvier_runtime::Axon;

// ---- Shared fixtures -------------------------------------------------------

/// Minimal transition that echoes the static string "ok".
#[derive(Clone, Default)]
struct OkTransition;

#[async_trait::async_trait]
impl Transition<(), String> for OkTransition {
    type Error = Infallible;
    type Resources = ();

    async fn run(
        &self,
        _state: (),
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        Outcome::next("ok".to_string())
    }
}

fn ok_circuit() -> Axon<(), String, Infallible, ()> {
    Axon::new("ok").then(OkTransition)
}

// ---- Tests -----------------------------------------------------------------

/// Basic prefix: RouteGroup::new("/api").get("", …) → GET /api  => 200
#[tokio::test]
async fn route_group_prefix_empty_sub_path() {
    let ingress = Ranvier::http::<()>().route_group(RouteGroup::new("/api").get("", ok_circuit()));

    let app = TestApp::new(ingress, ());

    // Exact prefix match: /api
    let res = app
        .send(TestRequest::get("/api"))
        .await
        .expect("request should succeed");
    assert_eq!(res.status(), StatusCode::OK, "GET /api expected 200");

    // Sub-path that doesn't exist should 404
    let res2 = app
        .send(TestRequest::get("/api/other"))
        .await
        .expect("request should succeed");
    assert_eq!(
        res2.status(),
        StatusCode::NOT_FOUND,
        "GET /api/other expected 404"
    );
}

/// Prefix + sub-path: RouteGroup::new("/api").get("/users", …) → GET /api/users
#[tokio::test]
async fn route_group_prefix_with_sub_path() {
    let ingress = Ranvier::http::<()>().route_group(
        RouteGroup::new("/api/v1")
            .get("/users", ok_circuit())
            .post("/users", ok_circuit()),
    );

    let app = TestApp::new(ingress, ());

    let res = app.send(TestRequest::get("/api/v1/users")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK, "GET /api/v1/users");

    let res = app.send(TestRequest::post("/api/v1/users")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK, "POST /api/v1/users");

    // Wrong method
    let res = app
        .send(TestRequest::delete("/api/v1/users"))
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::NOT_FOUND,
        "DELETE /api/v1/users should 404"
    );
}

/// Nested groups: parent /api, child /v1  →  /api/v1/ping
#[tokio::test]
async fn route_group_nested_group() {
    let ingress = Ranvier::http::<()>().route_group(
        RouteGroup::new("/api").group(RouteGroup::new("/v1").get("/ping", ok_circuit())),
    );

    let app = TestApp::new(ingress, ());

    let res = app.send(TestRequest::get("/api/v1/ping")).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "GET /api/v1/ping (nested group)"
    );

    let res = app.send(TestRequest::get("/api/ping")).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::NOT_FOUND,
        "GET /api/ping should 404 (wrong prefix)"
    );
}

/// Multiple groups on the same ingress stay independent.
#[tokio::test]
async fn route_group_multiple_groups_on_ingress() {
    let ingress = Ranvier::http::<()>()
        .route_group(
            RouteGroup::new("/users")
                .get("", ok_circuit())
                .post("", ok_circuit()),
        )
        .route_group(RouteGroup::new("/orders").get("", ok_circuit()));

    let app = TestApp::new(ingress, ());

    assert_eq!(
        app.send(TestRequest::get("/users")).await.unwrap().status(),
        StatusCode::OK
    );
    assert_eq!(
        app.send(TestRequest::post("/users"))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        app.send(TestRequest::get("/orders"))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    // Cross-group non-existent path
    assert_eq!(
        app.send(TestRequest::get("/products"))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
}

/// RouteGroup + plain .route() interop: both should work on the same ingress.
#[tokio::test]
async fn route_group_alongside_plain_route() {
    let ingress = Ranvier::http::<()>()
        .route("/ping", ok_circuit())
        .route_group(RouteGroup::new("/api").get("/status", ok_circuit()));

    let app = TestApp::new(ingress, ());

    assert_eq!(
        app.send(TestRequest::get("/ping")).await.unwrap().status(),
        StatusCode::OK,
        "plain .route() should still work"
    );
    assert_eq!(
        app.send(TestRequest::get("/api/status"))
            .await
            .unwrap()
            .status(),
        StatusCode::OK,
        "route_group route should work alongside plain route"
    );
}

/// Deeply nested: /a/b/c  (3 levels)
#[tokio::test]
async fn route_group_deeply_nested() {
    let ingress = Ranvier::http::<()>().route_group(
        RouteGroup::new("/a")
            .group(RouteGroup::new("/b").group(RouteGroup::new("/c").get("", ok_circuit()))),
    );

    let app = TestApp::new(ingress, ());
    let res = app.send(TestRequest::get("/a/b/c")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK, "GET /a/b/c (3 levels deep)");
}

/// Trailing slash normalisation: RouteGroup::new("/api/") matches /api
#[tokio::test]
async fn route_group_trailing_slash_normalised() {
    let ingress = Ranvier::http::<()>().route_group(RouteGroup::new("/api/").get("", ok_circuit()));

    let app = TestApp::new(ingress, ());
    let res = app.send(TestRequest::get("/api")).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "trailing slash in prefix should normalise"
    );
}
