use ranvier::prelude::*;
use ranvier_macros::transition;
use serde::{Deserialize, Serialize};
use jsonwebtoken::{decode, DecodingKey, Validation, Algorithm};

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Claims {
    sub: String,
    roles: Vec<String>,
    exp: usize,
}

/// Transition that verifies JWT token from Authorization header in Bus.
#[transition]
async fn verify_token(_input: (), _res: &(), bus: &mut Bus) -> Outcome<Claims, String> {
    let headers: http::HeaderMap = bus.read::<http::HeaderMap>().cloned().unwrap_or_default();
    let bearer: Option<&str> = headers
        .get("authorization")
        .and_then(|v: &http::HeaderValue| v.to_str().ok())
        .and_then(|v: &str| v.strip_prefix("Bearer "));

    let token = match bearer {
        Some(t) => t.to_string(),
        None => return Outcome::Fault("Missing Authorization header".to_string()),
    };

    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_exp = true;

    match decode::<Claims>(
        &token,
        &DecodingKey::from_secret(b"bench-secret-key"),
        &validation,
    ) {
        Ok(data) => {
            if !data.claims.roles.contains(&"admin".to_string()) {
                return Outcome::Fault("Forbidden: admin role required".to_string());
            }
            Outcome::Next(data.claims)
        }
        Err(e) => Outcome::Fault(format!("JWT verification failed: {}", e)),
    }
}

#[transition]
async fn final_response(claims: Claims, _res: &(), _bus: &mut Bus) -> Outcome<serde_json::Value, String> {
    Outcome::Next(serde_json::json!({
        "subject": claims.sub,
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
    println!("Starting Ranvier Benchmark Server (Scenario 2: Auth Flow) on {}", addr);

    let auth_axon = Axon::<(), (), String>::new("auth-flow")
        .then(verify_token)
        .then(final_response);

    Ranvier::http()
        .bind(addr)
        .route("/protected", auth_axon)
        .run(())
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    Ok(())
}

fn generate_bench_token(secret: &str) -> anyhow::Result<String> {
    use jsonwebtoken::{encode, Header, EncodingKey};
    use std::time::{SystemTime, UNIX_EPOCH};

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
