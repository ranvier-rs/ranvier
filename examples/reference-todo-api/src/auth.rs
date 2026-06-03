use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use ranvier_core::prelude::Bus;
use serde::{Deserialize, Serialize};

/// JWT secret loaded from the `JWT_SECRET` environment variable.
///
/// Run with: `JWT_SECRET=your-secret-here cargo run --example reference-todo-api`
fn jwt_secret() -> Result<String, String> {
    std::env::var("JWT_SECRET").map_err(|_| {
        "JWT_SECRET environment variable must be set. \
         Example: JWT_SECRET=your-secret-here cargo run"
            .to_string()
    })
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Claims {
    pub sub: String,
    pub exp: usize,
}

#[derive(Debug, Clone)]
pub struct AuthFailure(pub String);

pub fn issue_token(username: &str) -> Result<String, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs() as usize;

    let claims = Claims {
        sub: username.to_string(),
        exp: now + 24 * 3600,
    };
    let secret = jwt_secret()?;

    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|e| e.to_string())
}

pub fn verify_token(token: &str) -> Result<Claims, String> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_exp = true;
    let secret = jwt_secret()?;

    decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .map(|data| data.claims)
    .map_err(|e| format!("JWT verification failed: {}", e))
}

/// Extract bearer token from an Authorization header value.
pub fn extract_bearer(header_value: &str) -> Option<&str> {
    header_value.strip_prefix("Bearer ")
}

pub fn inject_auth_from_headers(parts: &http::request::Parts, bus: &mut Bus) {
    let Some(header) = parts.headers.get(http::header::AUTHORIZATION) else {
        return;
    };

    let header_value = match header.to_str() {
        Ok(value) => value,
        Err(_) => {
            bus.insert(AuthFailure(
                "Invalid Authorization header encoding".to_string(),
            ));
            return;
        }
    };

    let Some(token) = extract_bearer(header_value) else {
        bus.insert(AuthFailure(
            "Authorization header must use Bearer token format".to_string(),
        ));
        return;
    };

    match verify_token(token) {
        Ok(claims) => bus.insert(claims),
        Err(error) => bus.insert(AuthFailure(error)),
    }
}

pub fn require_claims(bus: &Bus) -> Result<Claims, String> {
    if let Ok(failure) = bus.get_cloned::<AuthFailure>() {
        return Err(failure.0);
    }

    bus.get_cloned::<Claims>()
        .map_err(|_| "Missing Authorization header".to_string())
}
