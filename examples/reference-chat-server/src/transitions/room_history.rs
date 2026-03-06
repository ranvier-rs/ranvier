use crate::ws::room_manager::RoomManager;
use ranvier_core::Outcome;
use ranvier_macros::transition;

#[transition]
pub async fn room_history(
    _input: (),
    _res: &(),
    bus: &mut ranvier_core::Bus,
) -> Outcome<serde_json::Value, String> {
    let room_id = bus.read::<String>().cloned().unwrap_or_default();
    if room_id.is_empty() {
        return Outcome::Fault("room_id path parameter is required".to_string());
    }

    let room_manager = bus.read::<RoomManager>().cloned().expect("RoomManager");
    let messages = room_manager.get_history(&room_id, 100);

    Outcome::Next(serde_json::json!({
        "room_id": room_id,
        "messages": messages
    }))
}
