use ranvier::prelude::*;
use ranvier_auth::prelude::*;
use ranvier_macros::transition;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct AuthResponse {
    subject: String,
    message: String,
}

/// Transition that explicitly reads AuthContext from the Bus.
/// This highlights the "Typed Decision Flow" where auth data is first-class.
#[transition]
async fn check_capability(_input: (), _res: &(), bus: &mut Bus) -> Outcome<AuthContext, anyhow::Error> {
    if let Some(ctx) = auth_context(bus) {
        Outcome::Next(ctx.clone())
    } else {
        Outcome::Fault(anyhow::anyhow!("Unauthorized in Bus"))
    }
}

#[transition]
async fn final_response(ctx: AuthContext, _res: &(), _bus: &mut Bus) -> Outcome<serde_json::Value, anyhow::Error> {
    Outcome::Next(serde_json::json!({
        "subject": ctx.subject,
        "message": "Access Granted via Typed Bus Capability"
    }))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let token_secret = "bench-secret-key";

    if args.len() > 1 && args[1] == "gen-token" {
        let token = generate_bench_token(token_secret)?;
        print!("{}", token);
        return Ok(());
    }

    let addr = "0.0.0.0:3001";
    println!("Starting Ranvier Benchmark Server (Scenario 2: Complex Auth) on {}", addr);

    let auth_axon = Axon::<(), (), anyhow::Error>::new("auth-flow")
        .then(check_capability)
        .then(final_response);

    Ranvier::http()
        .bind(addr)
        .bus_injector(|req, bus| {
            inject_auth_context(req, bus);
        })
        .layer(BearerAuthLayer::new_hs256(token_secret).required())
        .layer(RequireRoleLayer::new("admin"))
        .route("/protected", auth_axon)
        .run(())
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    Ok(())
}

fn generate_bench_token(secret: &str) -> anyhow::Result<String> {
    use jsonwebtoken::{encode, Header, EncodingKey, Algorithm};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[derive(Serialize)]
    struct Claims {
        sub: String,
        roles: Vec<String>,
        exp: usize,
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_secs() as usize;

    let claims = Claims {
        sub: "bench-user".to_string(),
        roles: vec!["admin".to_string(), "user".to_string()],
        exp: now + 3600,
    };

    let token = encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )?;

    Ok(token)
}
