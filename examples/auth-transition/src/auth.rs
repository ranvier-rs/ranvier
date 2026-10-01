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
    if secret.trim().is_empty() || token.is_empty() || token.len() > 8192 {
        return Err(AuthError::InvalidToken("Authentication required".into()));
    }
    use jsonwebtoken::{DecodingKey, Validation, decode};

    let key = DecodingKey::from_secret(secret.as_bytes());
    let validation = Validation::default();

    let token_data = decode::<Claims>(token, &key, &validation).map_err(|e| {
        if matches!(e.kind(), jsonwebtoken::errors::ErrorKind::ExpiredSignature) {
            AuthError::ExpiredToken
        } else {
            AuthError::InvalidToken("Authentication required".into())
        }
    })?;

    if token_data.claims.sub.trim().is_empty() {
        return Err(AuthError::InvalidToken("Authentication required".into()));
    }

    Ok(AuthContext {
        user_id: token_data.claims.sub,
        roles: token_data.claims.roles,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signed_expired_token_is_typed_and_invalid_credentials_are_fixed_errors() {
        let secret = "explicit-local-test-secret";
        let expired = Claims {
            sub: "alice".into(),
            roles: vec!["admin".into()],
            exp: (chrono::Utc::now().timestamp() - 120) as usize,
        };
        let token = jsonwebtoken::encode(
            &jsonwebtoken::Header::default(),
            &expired,
            &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap();
        assert!(matches!(
            validate_jwt(&token, secret),
            Err(AuthError::ExpiredToken)
        ));
        for (token, secret) in [
            (token.as_str(), "other-secret"),
            ("invalid", secret),
            ("invalid", " "),
        ] {
            assert!(
                matches!(validate_jwt(token, secret), Err(AuthError::InvalidToken(ref message)) if message == "Authentication required")
            );
        }
        assert!(validate_jwt(&"a".repeat(8193), secret).is_err());
    }
}
