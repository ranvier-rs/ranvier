use ranvier_core::prelude::*;
use ranvier_macros::transition;
use crate::models::{CreateTodoRequest, Todo};
use std::sync::{Arc, Mutex};

#[transition]
pub async fn create_todo(
    _input: (),
    _res: &(),
    bus: &mut Bus,
) -> Outcome<serde_json::Value, String> {
    let body = bus.read::<String>().cloned().unwrap_or_default();
    let request: CreateTodoRequest = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(_) => return Outcome::Fault("Invalid JSON body".to_string()),
    };

    if request.title.trim().is_empty() {
        return Outcome::Fault("Title cannot be empty".to_string());
    }

    let todo = Todo::new(request.title);

    // Store in shared state via Bus
    if let Some(store) = bus.read::<Arc<Mutex<Vec<Todo>>>>() {
        let mut todos = store.lock().unwrap();
        todos.push(todo.clone());
    }

    Outcome::Next(serde_json::to_value(todo).unwrap())
}
