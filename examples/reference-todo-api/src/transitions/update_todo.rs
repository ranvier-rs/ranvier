use crate::auth;
use crate::models::{Todo, UpdateTodoRequest};
use ranvier_core::prelude::*;
use ranvier_http::BusHttpExt;
use ranvier_macros::transition;
use std::sync::{Arc, Mutex};

/// Update todo transition — receives `UpdateTodoRequest` directly via `put_typed()`.
///
/// - Body (`UpdateTodoRequest`) is auto-deserialized by `put_typed()` as the Axon input
/// - Path param `:id` is read via `BusHttpExt::path_param()`
#[transition]
pub async fn update_todo(
    request: UpdateTodoRequest,
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
    if let Some(todo) = todos.iter_mut().find(|t| t.id == id) {
        if let Some(title) = request.title {
            todo.title = title;
        }
        if let Some(completed) = request.completed {
            todo.completed = completed;
        }
        return match serde_json::to_value(todo.clone()) {
            Ok(value) => Outcome::Next(value),
            Err(error) => Outcome::Fault(format!("Todo serialization failed: {error}")),
        };
    }

    Outcome::Fault(format!("Todo not found: {}", id))
}
