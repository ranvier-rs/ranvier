//! Bearer token authentication for Inspector production deployments.
//!
//! Enable by calling `Inspector::with_bearer_token("secret-token")`.
//! When enabled, all API requests must include `Authorization: Bearer <token>`.
//! Unauthenticated requests receive 401 Unauthorized.

use axum::http::{HeaderMap, StatusCode};
use serde_json::Value;
use subtle::ConstantTimeEq;

/// Bearer token authentication configuration.
#[derive(Clone, Debug, Default)]
pub struct BearerAuth {
    /// The expected bearer token. If None, bearer auth is disabled.
    pub token: Option<String>,
}

impl BearerAuth {
    /// Create a new BearerAuth from environment variable `RANVIER_INSPECTOR_TOKEN`.
    pub fn from_env() -> Self {
        Self {
            token: normalize_token(std::env::var("RANVIER_INSPECTOR_TOKEN").ok()),
        }
    }

    /// Check if bearer auth is enabled.
    pub fn is_enabled(&self) -> bool {
        self.expected_token().is_some()
    }

    /// Validate the Authorization header against the configured token.
    /// Returns Ok(()) if auth passes, Err with status code and error body if not.
    pub fn validate(&self, headers: &HeaderMap) -> Result<(), (StatusCode, axum::Json<Value>)> {
        let Some(expected) = self.expected_token() else {
            return Ok(()); // Auth not enabled
        };

        let auth_header = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if let Some(token) = auth_header.strip_prefix("Bearer ") {
            let provided = token.trim().as_bytes();
            let expected_bytes = expected.as_bytes();
            // Use constant-time comparison to prevent timing attacks.
            if provided.len() == expected_bytes.len() && provided.ct_eq(expected_bytes).into() {
                return Ok(());
            }
            return Err((
                StatusCode::UNAUTHORIZED,
                axum::Json(serde_json::json!({
                    "error": "invalid_bearer_token",
                    "message": "The provided bearer token is invalid"
                })),
            ));
        }

        Err((
            StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({
                "error": "missing_bearer_token",
                "message": "Authorization: Bearer <token> header is required"
            })),
        ))
    }

    fn expected_token(&self) -> Option<&str> {
        self.token
            .as_deref()
            .map(str::trim)
            .filter(|token| !token.is_empty())
    }
}

pub(crate) fn normalize_token(token: Option<String>) -> Option<String> {
    token
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
}
