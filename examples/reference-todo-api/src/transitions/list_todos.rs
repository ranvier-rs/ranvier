use crate::auth;
use crate::models::Todo;
use ranvier_core::prelude::*;
use ranvier_macros::transition;
use std::sync::{Arc, Mutex};

#[transition]
pub async fn list_todos(
    _input: (),
    _res: &(),
    bus: &mut Bus,
) -> Outcome<serde_json::Value, String> {
    if let Err(error) = auth::require_claims(bus) {
        return Outcome::Fault(error);
    }

    let store = match bus.get_cloned::<Arc<Mutex<Vec<Todo>>>>() {
        Ok(store) => store,
        Err(_) => return Outcome::Fault("Todo store unavailable".to_string()),
    };

    let todos = match store.lock() {
        Ok(todos) => todos.clone(),
        Err(_) => return Outcome::Fault("Todo store lock poisoned".to_string()),
    };

    match serde_json::to_value(todos) {
        Ok(value) => Outcome::Next(value),
        Err(error) => Outcome::Fault(format!("Todo list serialization failed: {error}")),
    }
}
