//! # Large API Demo — Route/Guard Visibility
//!
//! Optional proof surface for grouped routes, nested group boundaries, and
//! effective guard-stack introspection via `route_descriptors()`.
//!
//! Run:
//! `cargo run -p ranvier-http --example large_api_demo`

use http::StatusCode;
use ranvier_core::{Bus, Outcome, Transition};
use ranvier_guard::prelude::*;
use ranvier_http::prelude::*;
use ranvier_runtime::Axon;

#[derive(Clone, Default)]
struct Ok200;

#[async_trait::async_trait]
impl Transition<(), String> for Ok200 {
    type Error = String;
    type Resources = ();

    async fn run(&self, _: (), _: &(), _bus: &mut Bus) -> Outcome<String, String> {
        Outcome::next("ok".to_string())
    }
}

fn handler() -> Axon<(), String, String, ()> {
    Axon::simple::<String>("ok").then(Ok200)
}

fn crud_group(group: RouteGroup<()>, prefix: &str) -> RouteGroup<()> {
    group.group(prefix, |g| {
        g.get_json_out("", handler())
            .post_json_out("", handler())
            .get_json_out("/:id", handler())
            .put("/:id", handler())
            .delete_json_out("/:id", handler())
    })
}

fn build_router() -> HttpIngress<()> {
    Ranvier::http::<()>()
        .guard(AccessLogGuard::<()>::new())
        .get_json_out("/ping", handler())
        .group("/api/v1", |g| {
            let g = g.guard(RequestIdGuard::<()>::new());
            let g = crud_group(g, "/users");
            let g = crud_group(g, "/orders");
            let g = crud_group(g, "/products");
            g.group("/admin", |admin| {
                admin
                    .guard(AuthGuard::<()>::bearer(vec!["admin-token".into()]))
                    .get_json_out("/users", handler())
                    .post_json_out("/users", handler())
                    .get_json_out("/metrics", handler())
            })
        })
        .group("/api/v2", |g| {
            let g = g.guard(RequestIdGuard::<()>::new());
            let g = crud_group(g, "/users");
            let g = crud_group(g, "/orders");
            g.get_json_out("/search", handler())
                .get_json_out("/analytics/summary", handler())
        })
        .group("/admin", |g| {
            g.guard(AuthGuard::<()>::bearer(vec!["platform-admin".into()]))
                .get_json_out("/health", handler())
                .get_json_out("/metrics", handler())
        })
}

fn print_descriptor_summary(ingress: &HttpIngress<()>) {
    let descriptors = ingress.route_descriptors();
    println!(
        "=== Route / Guard Visibility Demo ({}) ===",
        descriptors.len()
    );
    for descriptor in &descriptors {
        println!("{:6} {}", descriptor.method(), descriptor.path_pattern());
        for guard in descriptor.guard_descriptors() {
            let scope = match guard.scope() {
                HttpGuardScope::Global => "global".to_string(),
                HttpGuardScope::Group => {
                    format!("group {}", guard.scope_path().unwrap_or("<none>"))
                }
                HttpGuardScope::Route => {
                    format!("route {}", guard.scope_path().unwrap_or("<none>"))
                }
            };
            println!("        - {} [{}]", guard.name(), scope);
        }
    }

    let admin_v1 = descriptors
        .iter()
        .find(|descriptor| descriptor.path_pattern() == "/api/v1/admin/users")
        .expect("nested admin route should exist");
    assert_eq!(admin_v1.method(), &http::Method::GET);
    assert_eq!(admin_v1.guard_descriptors().len(), 3);
    assert_eq!(admin_v1.guard_descriptors()[0].name(), "AccessLogGuard");
    assert_eq!(admin_v1.guard_descriptors()[1].name(), "RequestIdGuard");
    assert_eq!(admin_v1.guard_descriptors()[2].name(), "AuthGuard");

    let public_ping = descriptors
        .iter()
        .find(|descriptor| descriptor.path_pattern() == "/ping")
        .expect("public ping route should exist");
    assert_eq!(public_ping.method(), &http::Method::GET);
    assert_eq!(public_ping.guard_descriptors().len(), 1);
    assert_eq!(public_ping.guard_descriptors()[0].name(), "AccessLogGuard");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ingress = build_router();
    print_descriptor_summary(&ingress);

    let app = TestApp::new(ingress, ());
    let response = app
        .send(TestRequest::get("/api/v1/users"))
        .await
        .expect("request should succeed");
    assert_eq!(response.status(), StatusCode::OK);

    println!("Visibility demo verified against TestApp request path.");
    Ok(())
}
