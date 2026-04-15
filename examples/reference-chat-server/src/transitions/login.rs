use crate::auth::{self, TokenStore};
use ranvier_core::Outcome;
use ranvier_macros::transition;

#[transition]
pub async fn login(
    input: serde_json::Value,
    _res: &(),
    bus: &mut ranvier_core::Bus,
) -> Outcome<serde_json::Value, String> {
    let username = input.get("username").and_then(|v| v.as_str()).unwrap_or("");

    if username.is_empty() {
        return Outcome::Fault("username is required".to_string());
    }

    let token_store = bus
        .get_cloned::<TokenStore>()
        .expect("TokenStore must be injected");

    let user_id = uuid::Uuid::new_v4().to_string();
    let token = auth::issue_token(&token_store, &user_id, username);

    Outcome::Next(serde_json::json!({
        "token": token,
        "user_id": user_id,
        "username": username
    }))
}
