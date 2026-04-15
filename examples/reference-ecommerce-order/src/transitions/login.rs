use crate::auth;
use crate::models::LoginReq;
use ranvier_core::prelude::*;
use ranvier_macros::transition;

/// Login transition — receives `LoginReq` directly via `post_typed()`.
///
/// No manual JSON parsing needed: the HTTP ingress auto-deserializes
/// the request body and passes it as the typed Axon input.
#[transition]
pub async fn login(req: LoginReq, _res: &(), _bus: &mut Bus) -> Outcome<serde_json::Value, String> {
    // Demo credentials
    if req.username == "merchant" && req.password == "merchant123" {
        let token = auth::create_token(&req.username, &req.tenant_id, "merchant");
        Outcome::Next(serde_json::json!({
            "token": token,
            "username": req.username,
            "tenant_id": req.tenant_id,
            "role": "merchant"
        }))
    } else {
        Outcome::Fault("Invalid credentials".to_string())
    }
}
