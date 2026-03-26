use crate::auth::{self, TokenStore};
use crate::ws::room_manager::RoomManager;
use ranvier_core::Outcome;
use ranvier_http::prelude::*;
use ranvier_macros::transition;

#[transition]
pub async fn create_room(
    _input: (),
    _res: &(),
    bus: &mut ranvier_core::Bus,
) -> Outcome<serde_json::Value, String> {
    // Verify auth
    let token_store = bus.get_cloned::<TokenStore>().expect("TokenStore");
    let auth_header = bus.get_cloned::<String>().unwrap_or_default();
    let token = auth::extract_bearer(&auth_header).unwrap_or("");
    let claims = match auth::verify_token(&token_store, token) {
        Some(c) => c,
        None => return Outcome::Fault("Unauthorized".to_string()),
    };

    let body = match bus.get_cloned::<Json<serde_json::Value>>() {
        Ok(json) => json.0,
        Err(_) => return Outcome::Fault("Missing JSON body".to_string()),
    };

    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
    if name.is_empty() {
        return Outcome::Fault("name is required".to_string());
    }
    let is_public = body.get("is_public").and_then(|v| v.as_bool()).unwrap_or(true);

    let room_manager = bus.get_cloned::<RoomManager>().expect("RoomManager");
    let room = room_manager.create_room(name, &claims.user_id, is_public);

    Outcome::Next(serde_json::json!({
        "id": room.id,
        "name": room.name,
        "is_public": room.is_public,
        "created_by": room.created_by
    }))
}
