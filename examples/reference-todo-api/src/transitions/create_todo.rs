use ranvier_core::prelude::*;
use ranvier_macros::transition;
use crate::models::{CreateTodoRequest, Todo};
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
    if request.title.trim().is_empty() {
        return Outcome::Fault("Title cannot be empty".to_string());
    }

    let todo = Todo::new(request.title);

    // Store in shared state via Bus (injected by bus_injector)
    if let Ok(store) = bus.get_cloned::<Arc<Mutex<Vec<Todo>>>>() {
        let mut todos = store.lock().unwrap();
        todos.push(todo.clone());
    }

    Outcome::Next(serde_json::to_value(todo).unwrap())
}
