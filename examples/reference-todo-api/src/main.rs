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
//!
//! ## Prerequisites
//! - `hello-world` — basic Transition + Axon + HTTP ingress
//! - `macros-demo` — `#[transition]` macro usage
//!
//! ## Next Steps
//! - `reference-ecommerce-order` — saga compensation, audit, multi-tenancy
//! - `guard-demo` — Guard node pipeline patterns

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
        .post("/login", Axon::<(), (), String>::new("login").then(login))
        .get("/todos", Axon::<(), (), String>::new("list-todos").then(list_todos))
        .post("/todos", Axon::<(), (), String>::new("create-todo").then(create_todo))
        .get("/todos/:id", Axon::<(), (), String>::new("get-todo").then(get_todo))
        .put("/todos/:id", Axon::<(), (), String>::new("update-todo").then(update_todo))
        .delete("/todos/:id", Axon::<(), (), String>::new("delete-todo").then(delete_todo))
        .run(())
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    Ok(())
}
