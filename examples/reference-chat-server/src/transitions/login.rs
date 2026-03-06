use crate::auth::{self, TokenStore};
use ranvier_core::Outcome;
use ranvier_http::prelude::*;
use ranvier_macros::transition;

#[transition]
pub async fn login(
    _input: (),
    _res: &(),
    bus: &mut ranvier_core::Bus,
) -> Outcome<serde_json::Value, String> {
    let body = match bus.read::<Json<serde_json::Value>>() {
        Some(json) => json.0.clone(),
        None => return Outcome::Fault("Missing JSON body".to_string()),
    };

    let username = body
        .get("username")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if username.is_empty() {
        return Outcome::Fault("username is required".to_string());
    }

    let token_store = bus
        .read::<TokenStore>()
        .cloned()
        .expect("TokenStore must be injected");

    let user_id = uuid::Uuid::new_v4().to_string();
    let token = auth::issue_token(&token_store, &user_id, username);

    Outcome::Next(serde_json::json!({
        "token": token,
        "user_id": user_id,
        "username": username
    }))
}
