use axum::{
    extract::State,
    http::{header, StatusCode},
    middleware::{self, Next},
    response::Response,
    routing::get,
    Json, Router,
};
use jsonwebtoken::{decode, DecodingKey, Validation, Algorithm};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Claims {
    sub: String,
    roles: Vec<String>,
    exp: usize,
}

#[derive(Serialize)]
struct AuthResponse {
    subject: String,
    message: String,
}

#[derive(Clone)]
struct AppState {
    secret: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let token_secret = "bench-secret-key".to_string();

    if args.len() > 1 && args[1] == "gen-token" {
        let token = generate_bench_token(&token_secret)?;
        print!("{}", token);
        return Ok(());
    }

    let state = AppState {
        secret: token_secret,
    };

    let app = Router::new()
        .route("/protected", get(protected_handler))
        .layer(middleware::from_fn_with_state(state.clone(), auth_middleware))
        .with_state(state);

    let addr = "0.0.0.0:4001";
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("Starting Axum Benchmark Server (Scenario 2: Complex Auth) on {}", addr);
    axum::serve(listener, app).await?;

    Ok(())
}

async fn auth_middleware(
    State(state): State<AppState>,
    req: axum::extract::Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let auth_header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));

    let token = auth_header.ok_or(StatusCode::UNAUTHORIZED)?;

    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_exp = true;

    let token_data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(state.secret.as_bytes()),
        &validation,
    )
    .map_err(|_| StatusCode::UNAUTHORIZED)?;

    if !token_data.claims.roles.contains(&"admin".to_string()) {
        return Err(StatusCode::FORBIDDEN);
    }

    // Attach claims to request extensions (equivalent to Bus injection in Ranvier)
    let mut req = req;
    req.extensions_mut().insert(token_data.claims);

    Ok(next.run(req).await)
}

async fn protected_handler(
    axum::extract::Extension(claims): axum::extract::Extension<Claims>,
) -> Json<AuthResponse> {
    Json(AuthResponse {
        subject: claims.sub,
        message: "Access Granted via Axum Extensions".to_string(),
    })
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
