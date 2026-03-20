use ranvier_core::prelude::*;
use ranvier_macros::transition;
use crate::auth;
use crate::models::{LoginRequest, LoginResponse};

/// Login transition — receives `LoginRequest` directly via `post_typed()`.
///
/// No manual JSON parsing needed: the HTTP ingress auto-deserializes
/// the request body and passes it as the typed Axon input.
#[transition]
pub async fn login(
    request: LoginRequest,
    _res: &(),
    _bus: &mut Bus,
) -> Outcome<serde_json::Value, String> {
    // Simple hardcoded user check (demo purposes)
    if request.username == "admin" && request.password == "admin" {
        match auth::issue_token(&request.username) {
            Ok(token) => {
                let response = LoginResponse {
                    token,
                    username: request.username,
                };
                Outcome::Next(serde_json::to_value(response).unwrap())
            }
            Err(e) => Outcome::Fault(format!("Token generation failed: {}", e)),
        }
    } else {
        Outcome::Fault("Invalid credentials".to_string())
    }
}
