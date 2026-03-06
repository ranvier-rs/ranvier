use ranvier_core::prelude::*;
use ranvier_macros::transition;
use crate::models::Todo;
use std::sync::{Arc, Mutex};

#[transition]
pub async fn list_todos(
    _input: (),
    _res: &(),
    bus: &mut Bus,
) -> Outcome<serde_json::Value, String> {
    let todos = if let Some(store) = bus.read::<Arc<Mutex<Vec<Todo>>>>() {
        let todos = store.lock().unwrap();
        todos.clone()
    } else {
        vec![]
    };

    Outcome::Next(serde_json::to_value(todos).unwrap())
}
