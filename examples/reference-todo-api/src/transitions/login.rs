use ranvier_core::prelude::*;
use ranvier_macros::transition;
use crate::auth;
use crate::models::{LoginRequest, LoginResponse};

#[transition]
pub async fn login(
    _input: (),
    _res: &(),
    bus: &mut Bus,
) -> Outcome<serde_json::Value, String> {
    // Read the request body from Bus (injected by HTTP ingress)
    let body = bus.read::<String>().cloned().unwrap_or_default();
    let request: LoginRequest = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(_) => return Outcome::Fault("Invalid JSON body".to_string()),
    };

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
