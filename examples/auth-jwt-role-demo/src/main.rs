//! # JWT Role-Based Auth
//!
//! Demonstrates JWT bearer authentication with role-based access control.
//!
//! ## Run
//! ```bash
//! cargo run -p auth-jwt-role-demo
//! ```
//!
//! ## Key Concepts
//! - BearerAuthLayer for JWT validation
//! - RequireRoleLayer for role-based guards
//! - auth_context for extracting claims

use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use ranvier_auth::prelude::*;
use ranvier_core::prelude::*;
use ranvier_http::prelude::*;
use ranvier_runtime::Axon;
use serde::Serialize;

// SAFETY: demo-only fallback. In production, always set JWT_SECRET env var.
fn jwt_secret() -> String {
    std::env::var("JWT_SECRET").unwrap_or_else(|_| "ranvier-demo-secret".to_string())
}

#[derive(Clone)]
struct AdminGreeting;

#[async_trait::async_trait]
impl Transition<(), String> for AdminGreeting {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        _state: (),
        _resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        let greeting = match auth_context(bus) {
            Some(ctx) => format!("hello {}, admin access granted", ctx.subject),
            None => "no auth context found".to_string(),
        };
        Outcome::next(greeting)
    }
}

#[derive(Serialize)]
struct DemoClaims {
    sub: String,
    roles: Vec<String>,
    exp: usize,
}

fn issue_demo_admin_token() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("epoch")
        .as_secs() as usize;

    let claims = DemoClaims {
        sub: "demo-admin".to_string(),
        roles: vec!["admin".to_string()],
        exp: now + 60 * 60,
    };

    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(jwt_secret().as_bytes()),
    )
    .expect("token encode")
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let admin_token = issue_demo_admin_token();
    println!("demo admin token: Bearer {admin_token}");

    let secure_admin = Axon::<(), (), String, ()>::new("AdminGreeting").then(AdminGreeting);

    Ranvier::http::<()>()
        .bind("127.0.0.1:3107")
        .layer(RequireRoleLayer::new("admin"))
        // Global layers execute in LIFO order on request path.
        // Register role guard first so Bearer auth runs before role evaluation.
        .layer(BearerAuthLayer::new_hs256(&jwt_secret()).required())
        .bus_injector(inject_auth_context)
        .get("/admin", secure_admin)
        .run(())
        .await
}
