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

mod auth;
mod axons;
mod errors;
mod models;
mod store;
mod transitions;

use anyhow::Result;
use ranvier_http::Ranvier;
use ranvier_runtime::Axon;
use store::AppStore;

use transitions::{
    get_order::get_order,
    list_orders::list_orders,
    login::login,
};

fn login_circuit() -> Axon<(), serde_json::Value, String> {
    Axon::<(), (), String>::new("login").then(login)
}

fn list_orders_circuit() -> Axon<(), serde_json::Value, String> {
    Axon::<(), (), String>::new("list-orders").then(list_orders)
}

fn get_order_circuit() -> Axon<(), serde_json::Value, String> {
    Axon::<(), (), String>::new("get-order").then(get_order)
}

fn inventory_circuit() -> Axon<(), serde_json::Value, String> {
    use ranvier_core::prelude::*;
    use ranvier_macros::transition;

    #[transition]
    async fn list_inventory(
        _input: (),
        _res: &(),
        bus: &mut Bus,
    ) -> Outcome<serde_json::Value, String> {
        let inventory = bus
            .read::<AppStore>()
            .map(|s| s.get_inventory())
            .unwrap_or_default();
        Outcome::Next(serde_json::json!({ "inventory": inventory }))
    }

    Axon::<(), (), String>::new("list-inventory").then(list_inventory)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".to_string());
    let _store = AppStore::new();

    println!("Reference E-commerce Order API starting on {addr}");
    println!("  POST   /login             — authenticate (merchant/merchant123)");
    println!("  POST   /orders            — create order (4-stage saga pipeline)");
    println!("  GET    /orders            — list orders for tenant");
    println!("  GET    /orders/:id        — get single order");
    println!("  GET    /inventory         — list current inventory");
    println!();
    println!("Saga Pipeline: CreateOrder → ProcessPayment → ReserveInventory → ScheduleShipping");
    println!("Compensation:  RefundPayment ← ReleaseInventory ← (LIFO on failure)");

    Ranvier::http()
        .bind(&addr)
        .post("/login", login_circuit())
        .post("/orders", axons::order_pipeline::order_pipeline_circuit())
        .get("/orders", list_orders_circuit())
        .get("/orders/:id", get_order_circuit())
        .get("/inventory", inventory_circuit())
        .run(())
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    Ok(())
}
