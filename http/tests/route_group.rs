//! RouteGroup integration tests for the current `HttpIngress::group()` API.
//!
//! Validates prefix application, nested grouping, and scoped guard inheritance
//! using the active JSON-out route surface.

use http::StatusCode;
use ranvier_core::{Bus, Outcome, Transition};
use ranvier_guard::prelude::*;
use ranvier_http::guards;
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
        g.group("/b", |b| {
            b.group("/c", |c| c.get_json_out("", ok_circuit()))
        })
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

#[tokio::test]
async fn route_descriptors_capture_group_guard_scope_and_order() {
    let ingress = Ranvier::http::<()>()
        .guard(AccessLogGuard::<()>::new())
        .group("/api", |g| {
            g.guard(RequestIdGuard::<()>::new())
                .get_json_out("/status", ok_circuit())
                .group("/admin", |nested| {
                    nested
                        .guard(AuthGuard::<()>::bearer(vec!["admin-token".into()]))
                        .get_json_out("/users", ok_circuit())
                })
        })
        .get_json_out("/public", ok_circuit());

    let descriptors = ingress.route_descriptors();

    let public = descriptors
        .iter()
        .find(|descriptor| descriptor.path_pattern() == "/public")
        .expect("public route descriptor should exist");
    let public_guards = public.guard_descriptors();
    assert_eq!(public_guards.len(), 1);
    assert_eq!(public_guards[0].name(), "AccessLogGuard");
    assert!(matches!(public_guards[0].scope(), HttpGuardScope::Global));
    assert_eq!(public_guards[0].scope_path(), None);

    let status = descriptors
        .iter()
        .find(|descriptor| descriptor.path_pattern() == "/api/status")
        .expect("group route descriptor should exist");
    let status_guards = status.guard_descriptors();
    assert_eq!(status_guards.len(), 2);
    assert_eq!(status_guards[0].name(), "AccessLogGuard");
    assert!(matches!(status_guards[0].scope(), HttpGuardScope::Global));
    assert_eq!(status_guards[1].name(), "RequestIdGuard");
    assert!(matches!(status_guards[1].scope(), HttpGuardScope::Group));
    assert_eq!(status_guards[1].scope_path(), Some("/api"));

    let admin = descriptors
        .iter()
        .find(|descriptor| descriptor.path_pattern() == "/api/admin/users")
        .expect("nested group route descriptor should exist");
    let admin_guards = admin.guard_descriptors();
    assert_eq!(admin_guards.len(), 3);
    assert_eq!(admin_guards[0].name(), "AccessLogGuard");
    assert!(matches!(admin_guards[0].scope(), HttpGuardScope::Global));
    assert_eq!(admin_guards[1].name(), "RequestIdGuard");
    assert!(matches!(admin_guards[1].scope(), HttpGuardScope::Group));
    assert_eq!(admin_guards[1].scope_path(), Some("/api"));
    assert_eq!(admin_guards[2].name(), "AuthGuard");
    assert!(matches!(admin_guards[2].scope(), HttpGuardScope::Group));
    assert_eq!(admin_guards[2].scope_path(), Some("/api/admin"));
}

#[tokio::test]
async fn route_descriptors_capture_per_route_guards_without_leaking() {
    let ingress = Ranvier::http::<()>()
        .guard(AccessLogGuard::<()>::new())
        .get_with_guards(
            "/api/admin",
            ok_circuit(),
            guards![
                RequestIdGuard::<()>::new(),
                AuthGuard::<()>::bearer(vec!["admin-token".into()]),
            ],
        )
        .get_json_out("/api/public", ok_circuit());

    let descriptors = ingress.route_descriptors();

    let admin = descriptors
        .iter()
        .find(|descriptor| descriptor.path_pattern() == "/api/admin")
        .expect("per-route descriptor should exist");
    let admin_guards = admin.guard_descriptors();
    assert_eq!(admin_guards.len(), 3);
    assert_eq!(admin_guards[0].name(), "AccessLogGuard");
    assert!(matches!(admin_guards[0].scope(), HttpGuardScope::Global));
    assert_eq!(admin_guards[1].name(), "RequestIdGuard");
    assert!(matches!(admin_guards[1].scope(), HttpGuardScope::Route));
    assert_eq!(admin_guards[1].scope_path(), Some("/api/admin"));
    assert_eq!(admin_guards[2].name(), "AuthGuard");
    assert!(matches!(admin_guards[2].scope(), HttpGuardScope::Route));
    assert_eq!(admin_guards[2].scope_path(), Some("/api/admin"));

    let public = descriptors
        .iter()
        .find(|descriptor| descriptor.path_pattern() == "/api/public")
        .expect("plain route descriptor should exist");
    let public_guards = public.guard_descriptors();
    assert_eq!(public_guards.len(), 1);
    assert_eq!(public_guards[0].name(), "AccessLogGuard");
    assert!(matches!(public_guards[0].scope(), HttpGuardScope::Global));
}
