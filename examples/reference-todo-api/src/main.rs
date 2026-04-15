//! # Reference Todo API
//!
//! A complete CRUD application with JWT authentication built on Ranvier.
//!
//! ## Key v0.36 Features Demonstrated
//! - `post_typed()` / `put_typed()` — auto-deserialized JSON body as Axon input
//! - `Axon::typed::<T, E>()` — typed-input pipeline declaration
//! - `bus_injector()` — inject shared state + path params into Bus
//! - `PathParams` — type-safe path parameter extraction from Bus
//!
//! ## Run
//! ```bash
//! JWT_SECRET=your-secret cargo run -p reference-todo-api
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
//! - `closure-transition-demo` — `then_fn()`, `Axon::typed()`, `post_typed()`
//!
//! ## Next Steps
//! - `reference-ecommerce-order` — saga compensation, audit, multi-tenancy
//! - `guard-integration-demo` — Guard pipeline patterns

mod auth;
mod errors;
mod models;
mod transitions;

use anyhow::Result;
use models::Todo;
use ranvier_http::{PathParams, Ranvier};
use ranvier_runtime::Axon;
use std::sync::{Arc, Mutex};

use transitions::{
    create_todo::create_todo, delete_todo::delete_todo, get_todo::get_todo, list_todos::list_todos,
    login::login, update_todo::update_todo,
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

    // Shared in-memory store — injected into Bus via bus_injector
    let store: Arc<Mutex<Vec<Todo>>> = Arc::new(Mutex::new(Vec::new()));

    Ranvier::http()
        .bind(&addr)
        .bus_injector({
            let store = store.clone();
            move |parts: &http::request::Parts, bus: &mut ranvier_core::prelude::Bus| {
                // Inject shared store into Bus for all routes
                bus.insert(store.clone());
                // Inject path params into Bus for :id routes
                if let Some(params) = parts.extensions.get::<PathParams>() {
                    bus.insert(params.clone());
                }
            }
        })
        // Typed body routes — JSON auto-deserialized as Axon input
        .post_typed(
            "/login",
            Axon::typed::<models::LoginRequest, String>("login").then(login),
        )
        .post_typed(
            "/todos",
            Axon::typed::<models::CreateTodoRequest, String>("create-todo").then(create_todo),
        )
        .put_typed(
            "/todos/:id",
            Axon::typed::<models::UpdateTodoRequest, String>("update-todo").then(update_todo),
        )
        // Non-body routes — input is ()
        .get(
            "/todos",
            Axon::simple::<String>("list-todos").then(list_todos),
        )
        .get(
            "/todos/:id",
            Axon::simple::<String>("get-todo").then(get_todo),
        )
        .delete(
            "/todos/:id",
            Axon::simple::<String>("delete-todo").then(delete_todo),
        )
        .run(())
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    Ok(())
}
