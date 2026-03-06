use ranvier_core::prelude::*;
use ranvier_macros::transition;
use crate::auth;

#[transition]
pub async fn login(
    _input: (),
    _res: &(),
    bus: &mut Bus,
) -> Outcome<serde_json::Value, String> {
    let body = bus.read::<String>().cloned().unwrap_or_default();

    #[derive(serde::Deserialize)]
    struct LoginReq {
        username: String,
        password: String,
        tenant_id: String,
    }

    let req: LoginReq = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(_) => return Outcome::Fault("Invalid JSON: expected {username, password, tenant_id}".to_string()),
    };

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
