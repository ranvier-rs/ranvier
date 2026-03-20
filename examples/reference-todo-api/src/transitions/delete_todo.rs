use ranvier_core::prelude::*;
use ranvier_http::PathParams;
use ranvier_macros::transition;
use crate::models::Todo;
use std::sync::{Arc, Mutex};

/// Delete todo by ID — reads `:id` path param from `PathParams` in Bus.
#[transition]
pub async fn delete_todo(
    _input: (),
    _res: &(),
    bus: &mut Bus,
) -> Outcome<serde_json::Value, String> {
    // Read path param :id from PathParams (injected into Bus by bus_injector)
    let id: u64 = match bus.read::<PathParams>().and_then(|p| p.get("id")) {
        Some(id_str) => match id_str.parse() {
            Ok(id) => id,
            Err(_) => return Outcome::Fault("Invalid todo ID".to_string()),
        },
        None => return Outcome::Fault("Missing todo ID".to_string()),
    };

    if let Some(store) = bus.read::<Arc<Mutex<Vec<Todo>>>>() {
        let mut todos = store.lock().unwrap();
        let len_before = todos.len();
        todos.retain(|t| t.id != id);
        if todos.len() < len_before {
            return Outcome::Next(serde_json::json!({ "deleted": id }));
        }
    }

    Outcome::Fault(format!("Todo not found: {}", id))
}
