use crate::models::Todo;
use ranvier_core::prelude::*;
use ranvier_http::BusHttpExt;
use ranvier_macros::transition;
use std::sync::{Arc, Mutex};

/// Get todo by ID — reads `:id` path param via `BusHttpExt::path_param()`.
#[transition]
pub async fn get_todo(_input: (), _res: &(), bus: &mut Bus) -> Outcome<serde_json::Value, String> {
    let id: u64 = match bus.path_param("id") {
        Ok(id) => id,
        Err(e) => return Outcome::Fault(e),
    };

    if let Ok(store) = bus.get_cloned::<Arc<Mutex<Vec<Todo>>>>() {
        let todos = store.lock().unwrap();
        if let Some(todo) = todos.iter().find(|t| t.id == id) {
            return Outcome::Next(serde_json::to_value(todo).unwrap());
        }
    }

    Outcome::Fault(format!("Todo not found: {}", id))
}
