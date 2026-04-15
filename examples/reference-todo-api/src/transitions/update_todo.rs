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
    let id: u64 = match bus.path_param("id") {
        Ok(id) => id,
        Err(e) => return Outcome::Fault(e),
    };

    if let Ok(store) = bus.get_cloned::<Arc<Mutex<Vec<Todo>>>>() {
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
