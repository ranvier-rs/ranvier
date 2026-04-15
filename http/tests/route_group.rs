//! RouteGroup integration tests for the current `HttpIngress::group()` API.
//!
//! Validates prefix application, nested grouping, and scoped guard inheritance
//! using the active JSON-out route surface.

use http::StatusCode;
use ranvier_core::{Bus, Outcome, Transition};
use ranvier_http::prelude::*;
use ranvier_runtime::Axon;

#[derive(Clone, Default)]
struct OkTransition;

#[async_trait::async_trait]
impl Transition<(), String> for OkTransition {
    type Error = String;
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

fn ok_circuit() -> Axon<(), String, String, ()> {
    Axon::simple::<String>("ok").then(OkTransition)
}

#[tokio::test]
async fn group_prefix_empty_sub_path_maps_to_group_root() {
    let ingress = Ranvier::http::<()>().group("/api", |g| g.get_json_out("", ok_circuit()));

    let app = TestApp::new(ingress, ());

    let res = app
        .send(TestRequest::get("/api"))
        .await
        .expect("request should succeed");
    assert_eq!(res.status(), StatusCode::OK);

    let res2 = app
        .send(TestRequest::get("/api/other"))
        .await
        .expect("request should succeed");
    assert_eq!(res2.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn group_prefix_with_sub_path_maps_correctly() {
    let ingress = Ranvier::http::<()>().group("/api/v1", |g| {
        g.get_json_out("/users", ok_circuit())
            .post_json_out("/users", ok_circuit())
    });

    let app = TestApp::new(ingress, ());

    let res = app
        .send(TestRequest::get("/api/v1/users"))
        .await
        .expect("request should succeed");
    assert_eq!(res.status(), StatusCode::OK);

    let res = app
        .send(TestRequest::post("/api/v1/users"))
        .await
        .expect("request should succeed");
    assert_eq!(res.status(), StatusCode::OK);

    let res = app
        .send(TestRequest::delete("/api/v1/users"))
        .await
        .expect("request should succeed");
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn nested_groups_apply_combined_prefixes() {
    let ingress = Ranvier::http::<()>().group("/api", |g| {
        g.group("/v1", |nested| nested.get_json_out("/ping", ok_circuit()))
    });

    let app = TestApp::new(ingress, ());

    let res = app
        .send(TestRequest::get("/api/v1/ping"))
        .await
        .expect("request should succeed");
    assert_eq!(res.status(), StatusCode::OK);

    let res = app
        .send(TestRequest::get("/api/ping"))
        .await
        .expect("request should succeed");
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn multiple_groups_on_ingress_stay_independent() {
    let ingress = Ranvier::http::<()>()
        .group("/users", |g| {
            g.get_json_out("", ok_circuit())
                .post_json_out("", ok_circuit())
        })
        .group("/orders", |g| g.get_json_out("", ok_circuit()));

    let app = TestApp::new(ingress, ());

    assert_eq!(
        app.send(TestRequest::get("/users"))
            .await
            .expect("request should succeed")
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        app.send(TestRequest::post("/users"))
            .await
            .expect("request should succeed")
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        app.send(TestRequest::get("/orders"))
            .await
            .expect("request should succeed")
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        app.send(TestRequest::get("/products"))
            .await
            .expect("request should succeed")
            .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn group_routes_and_plain_routes_can_coexist() {
    let ingress = Ranvier::http::<()>()
        .get_json_out("/ping", ok_circuit())
        .group("/api", |g| g.get_json_out("/status", ok_circuit()));

    let app = TestApp::new(ingress, ());

    assert_eq!(
        app.send(TestRequest::get("/ping"))
            .await
            .expect("request should succeed")
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        app.send(TestRequest::get("/api/status"))
            .await
            .expect("request should succeed")
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn deeply_nested_groups_work_up_to_supported_depth() {
    let ingress = Ranvier::http::<()>().group("/a", |g| {
        g.group("/b", |b| b.group("/c", |c| c.get_json_out("", ok_circuit())))
    });

    let app = TestApp::new(ingress, ());
    let res = app
        .send(TestRequest::get("/a/b/c"))
        .await
        .expect("request should succeed");
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn trailing_slash_prefix_is_normalized_by_current_builder_path_join() {
    let ingress = Ranvier::http::<()>().group("/api/", |g| g.get_json_out("", ok_circuit()));

    let app = TestApp::new(ingress, ());
    let res = app
        .send(TestRequest::get("/api/"))
        .await
        .expect("request should succeed");
    assert_eq!(res.status(), StatusCode::OK);
}
