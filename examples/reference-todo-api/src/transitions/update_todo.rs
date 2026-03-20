use ranvier_core::prelude::*;
use ranvier_http::PathParams;
use ranvier_macros::transition;
use crate::models::{Todo, UpdateTodoRequest};
use std::sync::{Arc, Mutex};

/// Update todo transition — receives `UpdateTodoRequest` directly via `put_typed()`.
///
/// - Body (`UpdateTodoRequest`) is auto-deserialized by `put_typed()` as the Axon input
/// - Path param `:id` is read from `PathParams` in Bus (injected by `bus_injector`)
#[transition]
pub async fn update_todo(
    request: UpdateTodoRequest,
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
        if let Some(todo) = todos.iter_mut().find(|t| t.id == id) {
            if let Some(title) = request.title {
                todo.title = title;
            }
            if let Some(completed) = request.completed {
                todo.completed = completed;
            }
            return Outcome::Next(serde_json::to_value(todo.clone()).unwrap());
        }
    }

    Outcome::Fault(format!("Todo not found: {}", id))
}
