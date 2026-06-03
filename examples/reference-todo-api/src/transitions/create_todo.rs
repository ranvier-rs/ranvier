use crate::auth;
use crate::models::{CreateTodoRequest, Todo};
use ranvier_core::prelude::*;
use ranvier_macros::transition;
use std::sync::{Arc, Mutex};

/// Create todo transition — receives `CreateTodoRequest` directly via `post_typed()`.
///
/// No manual `serde_json::from_str` needed: the HTTP ingress auto-deserializes
/// the JSON body and passes the typed struct as the Axon input.
#[transition]
pub async fn create_todo(
    request: CreateTodoRequest,
    _res: &(),
    bus: &mut Bus,
) -> Outcome<serde_json::Value, String> {
    if let Err(error) = auth::require_claims(bus) {
        return Outcome::Fault(error);
    }

    if request.title.trim().is_empty() {
        return Outcome::Fault("Title cannot be empty".to_string());
    }

    let todo = Todo::new(request.title);

    // Store in shared state via Bus (injected by bus_injector)
    let store = match bus.get_cloned::<Arc<Mutex<Vec<Todo>>>>() {
        Ok(store) => store,
        Err(_) => return Outcome::Fault("Todo store unavailable".to_string()),
    };

    let mut todos = match store.lock() {
        Ok(todos) => todos,
        Err(_) => return Outcome::Fault("Todo store lock poisoned".to_string()),
    };
    todos.push(todo.clone());

    match serde_json::to_value(todo) {
        Ok(value) => Outcome::Next(value),
        Err(error) => Outcome::Fault(format!("Todo serialization failed: {error}")),
    }
}
