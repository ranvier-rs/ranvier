use ranvier_core::prelude::*;
use ranvier_http::BusHttpExt;
use ranvier_macros::transition;
use crate::models::Todo;
use std::sync::{Arc, Mutex};

/// Delete todo by ID — reads `:id` path param via `BusHttpExt::path_param()`.
#[transition]
pub async fn delete_todo(
    _input: (),
    _res: &(),
    bus: &mut Bus,
) -> Outcome<serde_json::Value, String> {
    let id: u64 = match bus.path_param("id") {
        Ok(id) => id,
        Err(e) => return Outcome::Fault(e),
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
