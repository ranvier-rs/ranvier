use ranvier_core::prelude::*;
use ranvier_macros::transition;
use crate::models::{Todo, UpdateTodoRequest};
use std::sync::{Arc, Mutex};

#[transition]
pub async fn update_todo(
    _input: (),
    _res: &(),
    bus: &mut Bus,
) -> Outcome<serde_json::Value, String> {
    let id_str = bus.read::<String>().cloned().unwrap_or_default();
    let id: u64 = match id_str.parse() {
        Ok(id) => id,
        Err(_) => return Outcome::Fault("Invalid todo ID".to_string()),
    };

    let body_str = bus.read::<String>().cloned().unwrap_or_default();
    let request: UpdateTodoRequest = match serde_json::from_str(&body_str) {
        Ok(r) => r,
        Err(_) => return Outcome::Fault("Invalid JSON body".to_string()),
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
