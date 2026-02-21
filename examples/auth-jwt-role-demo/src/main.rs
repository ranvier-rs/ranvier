use std::convert::Infallible;
use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use ranvier_auth::prelude::*;
use ranvier_core::prelude::*;
use ranvier_http::prelude::*;
use ranvier_runtime::Axon;
use serde::Serialize;

const JWT_SECRET: &str = "ranvier-demo-secret";

#[derive(Clone)]
struct AdminGreeting;

#[async_trait::async_trait]
impl Transition<(), String> for AdminGreeting {
    type Error = Infallible;
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
        &EncodingKey::from_secret(JWT_SECRET.as_bytes()),
    )
    .expect("token encode")
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let admin_token = issue_demo_admin_token();
    println!("demo admin token: Bearer {admin_token}");

    let secure_admin = Axon::<(), (), Infallible, ()>::new("AdminGreeting").then(AdminGreeting);

    Ranvier::http::<()>()
        .bind("127.0.0.1:3107")
        .layer(BearerAuthLayer::new_hs256(JWT_SECRET).required())
        .layer(RequireRoleLayer::new("admin"))
        .bus_injector(inject_auth_context)
        .get("/admin", secure_admin)
        .run(())
        .await
}
