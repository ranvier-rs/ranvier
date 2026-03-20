//! # Reference E-commerce Order Pipeline
//!
//! A complete order processing application demonstrating Ranvier's Saga compensation,
//! audit trail, multi-tenancy, and RFC 7807 error handling.
//!
//! ## Saga Flow
//! ```text
//! CreateOrder → ProcessPayment → ReserveInventory → ScheduleShipping
//!                  ↓ (comp)          ↓ (comp)
//!              RefundPayment    ReleaseInventory
//! ```
//!
//! ## Run
//! ```bash
//! cargo run -p reference-ecommerce-order
//! ```
//!
//! ## Endpoints
//! - POST   /login              — authenticate, returns JWT
//! - POST   /orders             — create order (4-stage saga pipeline)
//! - GET    /orders             — list orders for tenant
//! - GET    /orders/:id         — get single order
//! - GET    /inventory          — list current inventory
//!
//! ## Prerequisites
//! - `hello-world` — basic Transition + Axon + HTTP ingress
//! - `reference-todo-api` — CRUD patterns
//! - `auth-jwt-role-demo` — JWT authentication
//!
//! ## Next Steps
//! - `inspector-demo` — runtime observability for production pipelines
//! - `production-config-demo` — RanvierConfig for production deployment

mod auth;
mod axons;
mod errors;
mod models;
mod store;
mod transitions;

use anyhow::Result;
use ranvier_core::prelude::*;
use ranvier_http::{PathParams, Ranvier};
use ranvier_runtime::Axon;
use store::AppStore;

use transitions::{
    get_order::get_order,
    list_orders::list_orders,
    login::login,
};

/// Inventory listing uses a closure transition for simple read-only queries.
///
/// This demonstrates `then_fn()` — the lightweight alternative to `#[transition]`
/// for steps that don't need async or separate files.
fn inventory_circuit() -> Axon<(), serde_json::Value, String> {
    Axon::simple::<String>("list-inventory").then_fn("list-inventory", |_input: (), bus: &mut Bus| {
        let inventory = bus
            .read::<AppStore>()
            .map(|s| s.get_inventory())
            .unwrap_or_default();
        Outcome::Next(serde_json::json!({ "inventory": inventory }))
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".to_string());

    println!("Reference E-commerce Order API starting on {addr}");
    println!("  POST   /login             — authenticate (merchant/merchant123)");
    println!("  POST   /orders            — create order (4-stage saga pipeline)");
    println!("  GET    /orders            — list orders for tenant");
    println!("  GET    /orders/:id        — get single order");
    println!("  GET    /inventory         — list current inventory");
    println!();
    println!("Saga Pipeline: CreateOrder → ProcessPayment → ReserveInventory → ScheduleShipping");
    println!("Compensation:  RefundPayment ← ReleaseInventory ← (LIFO on failure)");

    let store = AppStore::new();

    // bus_injector bridges HTTP request parts into Bus:
    // - AppStore for order/inventory persistence
    // - PathParams for :id route parameters
    // - Authorization headers for JWT validation
    Ranvier::http()
        .bind(&addr)
        .bus_injector({
            let store = store.clone();
            move |parts: &http::request::Parts, bus: &mut Bus| {
                bus.insert(store.clone());
                if let Some(params) = parts.extensions.get::<PathParams>() {
                    bus.insert(params.clone());
                }
                // Inject headers for auth extraction in transitions
                let headers: Vec<(String, String)> = parts
                    .headers
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                    .collect();
                bus.insert(headers);
            }
        })
        .post_typed("/login", Axon::typed::<models::LoginReq, String>("login").then(login))
        .post_typed("/orders", axons::order_pipeline::order_pipeline_circuit())
        .get("/orders", Axon::simple::<String>("list-orders").then(list_orders))
        .get("/orders/:id", Axon::simple::<String>("get-order").then(get_order))
        .get("/inventory", inventory_circuit())
        .run(())
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    Ok(())
}
