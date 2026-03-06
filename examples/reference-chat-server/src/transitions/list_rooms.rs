use crate::ws::room_manager::RoomManager;
use ranvier_core::Outcome;
use ranvier_macros::transition;

#[transition]
pub async fn list_rooms(
    _input: (),
    _res: &(),
    bus: &mut ranvier_core::Bus,
) -> Outcome<serde_json::Value, String> {
    let room_manager = bus.read::<RoomManager>().cloned().expect("RoomManager");
    let rooms = room_manager.list_rooms();
    Outcome::Next(serde_json::json!({ "rooms": rooms }))
}
