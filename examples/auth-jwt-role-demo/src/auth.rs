//! JWT authentication module using `ranvier_core::iam`.
//!
//! Implements `IamVerifier` for HS256 JWT tokens with role-based claims.

use async_trait::async_trait;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use ranvier_core::iam::{IamError, IamIdentity, IamVerifier};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// JWT claims payload.
#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub roles: Vec<String>,
    pub exp: usize,
}

/// Issue a JWT token for the given user with roles.
impl JwtVerifier {
    pub fn from_secret(secret: &[u8]) -> Result<Self, &'static str> {
        if secret.is_empty() {
            return Err("JWT secret must not be empty");
        }
        Ok(Self {
            secret: Arc::from(secret),
        })
    }

    pub fn issue_token(&self, username: &str, roles: Vec<String>) -> Result<String, &'static str> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "Token clock is unavailable")?
            .as_secs();
        let expiry = now
            .checked_add(3600)
            .ok_or("Token expiry is out of range")?;
        let expiry = usize::try_from(expiry).map_err(|_| "Token expiry is out of range")?;

        let claims = Claims {
            sub: username.to_string(),
            roles,
            exp: expiry, // 1 hour
        };

        encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(&self.secret),
        )
        .map_err(|_| "Token issuance failed")
    }
}

/// JWT-based IamVerifier implementation.
///
/// Decodes HS256 tokens and maps JWT claims to `IamIdentity`.
#[derive(Clone)]
pub struct JwtVerifier {
    secret: Arc<[u8]>,
}

#[async_trait]
impl IamVerifier for JwtVerifier {
    async fn verify(&self, token: &str) -> Result<IamIdentity, IamError> {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.validate_exp = true;

        let token_data =
            decode::<Claims>(token, &DecodingKey::from_secret(&self.secret), &validation)
                .map_err(|_| IamError::InvalidToken("Authentication failed".into()))?;

        let claims = token_data.claims;

        let mut identity = IamIdentity::new(&claims.sub)
            .with_issuer("auth-jwt-role-demo")
            .with_roles(claims.roles);

        identity = identity.with_claim("exp", serde_json::json!(claims.exp));

        Ok(identity)
    }
}
