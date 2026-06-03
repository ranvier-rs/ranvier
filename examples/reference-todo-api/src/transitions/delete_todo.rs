use crate::auth;
use crate::models::Todo;
use ranvier_core::prelude::*;
use ranvier_http::BusHttpExt;
use ranvier_macros::transition;
use std::sync::{Arc, Mutex};

/// Delete todo by ID — reads `:id` path param via `BusHttpExt::path_param()`.
#[transition]
pub async fn delete_todo(
    _input: (),
    _res: &(),
    bus: &mut Bus,
) -> Outcome<serde_json::Value, String> {
    if let Err(error) = auth::require_claims(bus) {
        return Outcome::Fault(error);
    }

    let id: u64 = match bus.path_param("id") {
        Ok(id) => id,
        Err(e) => return Outcome::Fault(e),
    };

    let store = match bus.get_cloned::<Arc<Mutex<Vec<Todo>>>>() {
        Ok(store) => store,
        Err(_) => return Outcome::Fault("Todo store unavailable".to_string()),
    };

    let mut todos = match store.lock() {
        Ok(todos) => todos,
        Err(_) => return Outcome::Fault("Todo store lock poisoned".to_string()),
    };
    let len_before = todos.len();
    todos.retain(|t| t.id != id);
    if todos.len() < len_before {
        return Outcome::Next(serde_json::json!({ "deleted": id }));
    }

    Outcome::Fault(format!("Todo not found: {}", id))
}
