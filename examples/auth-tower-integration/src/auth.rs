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
#[allow(dead_code)]
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

    if secret.trim().is_empty() {
        return Err(AuthError::InvalidToken(
            "Invalid verifier configuration".into(),
        ));
    }
    let key = DecodingKey::from_secret(secret.as_bytes());
    let validation = Validation::default();

    let token_data = decode::<Claims>(token, &key, &validation).map_err(|e| {
        if matches!(e.kind(), jsonwebtoken::errors::ErrorKind::ExpiredSignature) {
            AuthError::ExpiredToken
        } else {
            AuthError::InvalidToken(e.to_string())
        }
    })?;

    if token_data.claims.sub.trim().is_empty() {
        return Err(AuthError::InvalidToken("Missing subject".into()));
    }

    Ok(AuthContext {
        user_id: token_data.claims.sub,
        roles: token_data.claims.roles,
    })
}
