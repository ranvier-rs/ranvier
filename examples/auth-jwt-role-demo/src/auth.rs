//! JWT authentication module using `ranvier_core::iam`.
//!
//! Implements `IamVerifier` for HS256 JWT tokens with role-based claims.

use async_trait::async_trait;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use ranvier_core::iam::{IamError, IamIdentity, IamVerifier};
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

/// JWT secret loaded from the `JWT_SECRET` environment variable.
///
/// # Panics
///
/// Panics at first access if the variable is not set.
/// Run with: `JWT_SECRET=your-secret-here cargo run --example auth-jwt-role-demo`
static JWT_SECRET: LazyLock<String> = LazyLock::new(|| {
    std::env::var("JWT_SECRET").expect(
        "JWT_SECRET environment variable must be set. \
         Example: JWT_SECRET=your-secret-here cargo run",
    )
});

/// JWT claims payload.
#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub roles: Vec<String>,
    pub exp: usize,
}

/// Issue a JWT token for the given user with roles.
pub fn issue_token(username: &str, roles: Vec<String>) -> Result<String, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs() as usize;

    let claims = Claims {
        sub: username.to_string(),
        roles,
        exp: now + 3600, // 1 hour
    };

    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(JWT_SECRET.as_bytes()),
    )
    .map_err(|e| e.to_string())
}

/// JWT-based IamVerifier implementation.
///
/// Decodes HS256 tokens and maps JWT claims to `IamIdentity`.
#[derive(Clone)]
pub struct JwtVerifier;

#[async_trait]
impl IamVerifier for JwtVerifier {
    async fn verify(&self, token: &str) -> Result<IamIdentity, IamError> {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.validate_exp = true;

        let token_data = decode::<Claims>(
            token,
            &DecodingKey::from_secret(JWT_SECRET.as_bytes()),
            &validation,
        )
        .map_err(|e| IamError::InvalidToken(format!("JWT decode failed: {}", e)))?;

        let claims = token_data.claims;

        let mut identity = IamIdentity::new(&claims.sub)
            .with_issuer("auth-jwt-role-demo")
            .with_roles(claims.roles);

        identity = identity.with_claim("exp", serde_json::json!(claims.exp));

        Ok(identity)
    }
}
