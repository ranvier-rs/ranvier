//! # Reference Todo API
//!
//! A complete CRUD application with JWT authentication built on Ranvier.
//!
//! ## Run
//! ```bash
//! cargo run -p reference-todo-api
//! ```
//!
//! ## Endpoints
//! - POST   /login          — authenticate (admin/admin), returns JWT
//! - GET    /todos          — list all todos
//! - POST   /todos          — create a todo
//! - GET    /todos/:id      — get a single todo
//! - PUT    /todos/:id      — update a todo
//! - DELETE /todos/:id      — delete a todo

mod auth;
mod errors;
mod models;
mod transitions;

use anyhow::Result;
use models::Todo;
use ranvier_http::Ranvier;
use ranvier_runtime::Axon;
use std::sync::{Arc, Mutex};

use transitions::{
    create_todo::create_todo,
    delete_todo::delete_todo,
    get_todo::get_todo,
    list_todos::list_todos,
    login::login,
    update_todo::update_todo,
};

fn login_circuit() -> Axon<(), serde_json::Value, String> {
    Axon::<(), (), String>::new("login").then(login)
}

fn list_circuit() -> Axon<(), serde_json::Value, String> {
    Axon::<(), (), String>::new("list-todos").then(list_todos)
}

fn create_circuit() -> Axon<(), serde_json::Value, String> {
    Axon::<(), (), String>::new("create-todo").then(create_todo)
}

fn get_circuit() -> Axon<(), serde_json::Value, String> {
    Axon::<(), (), String>::new("get-todo").then(get_todo)
}

fn update_circuit() -> Axon<(), serde_json::Value, String> {
    Axon::<(), (), String>::new("update-todo").then(update_todo)
}

fn delete_circuit() -> Axon<(), serde_json::Value, String> {
    Axon::<(), (), String>::new("delete-todo").then(delete_todo)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".to_string());
    println!("Reference Todo API starting on {addr}");
    println!("  POST   /login");
    println!("  GET    /todos");
    println!("  POST   /todos");
    println!("  GET    /todos/:id");
    println!("  PUT    /todos/:id");
    println!("  DELETE /todos/:id");

    // Shared in-memory store injected via Bus
    let _store: Arc<Mutex<Vec<Todo>>> = Arc::new(Mutex::new(Vec::new()));

    Ranvier::http()
        .bind(&addr)
        .post("/login", login_circuit())
        .get("/todos", list_circuit())
        .post("/todos", create_circuit())
        .get("/todos/:id", get_circuit())
        .put("/todos/:id", update_circuit())
        .delete("/todos/:id", delete_circuit())
        .run(())
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    Ok(())
}
