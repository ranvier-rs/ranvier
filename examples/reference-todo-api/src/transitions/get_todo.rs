use ranvier_core::prelude::*;
use ranvier_http::PathParams;
use ranvier_macros::transition;
use crate::models::Todo;
use std::sync::{Arc, Mutex};

/// Get todo by ID — reads `:id` path param from `PathParams` in Bus.
#[transition]
pub async fn get_todo(
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
        let todos = store.lock().unwrap();
        if let Some(todo) = todos.iter().find(|t| t.id == id) {
            return Outcome::Next(serde_json::to_value(todo).unwrap());
        }
    }

    Outcome::Fault(format!("Todo not found: {}", id))
}
