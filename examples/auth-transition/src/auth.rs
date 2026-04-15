use serde::{Deserialize, Serialize};

/// Authentication context containing user identity and roles.
/// Stored in Bus after successful authentication, available to all downstream transitions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthContext {
    pub user_id: String,
    pub roles: Vec<String>,
}

/// Authentication and authorization errors.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("Missing authorization header")]
    MissingHeader,

    #[error("Invalid token format")]
    InvalidFormat,

    #[error("Invalid token: {0}")]
    InvalidToken(String),

    #[error("Token expired")]
    ExpiredToken,

    #[error("Unauthorized: requires role {0}")]
    Unauthorized(String),
}

/// JWT claims structure matching our AuthContext.
#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: String, // user_id
    roles: Vec<String>,
    exp: usize, // expiration time
}

/// Validate JWT token and extract AuthContext.
///
/// This is a helper function (not a Transition) that performs JWT validation.
/// The actual Transition wraps this to return Outcome.
pub fn validate_jwt(token: &str, secret: &str) -> Result<AuthContext, AuthError> {
    use jsonwebtoken::{DecodingKey, Validation, decode};

    let key = DecodingKey::from_secret(secret.as_bytes());
    let validation = Validation::default();

    let token_data = decode::<Claims>(token, &key, &validation).map_err(|e| {
        if e.to_string().contains("ExpiredSignature") {
            AuthError::ExpiredToken
        } else {
            AuthError::InvalidToken(e.to_string())
        }
    })?;

    Ok(AuthContext {
        user_id: token_data.claims.sub,
        roles: token_data.claims.roles,
    })
}

/// Helper to create a test JWT token (for development/testing).
#[cfg(test)]
pub fn create_test_token(user_id: &str, roles: Vec<String>, secret: &str) -> String {
    use jsonwebtoken::{EncodingKey, Header, encode};

    let claims = Claims {
        sub: user_id.to_string(),
        roles,
        exp: (chrono::Utc::now() + chrono::Duration::hours(24)).timestamp() as usize,
    };

    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .expect("Failed to create test token")
}
