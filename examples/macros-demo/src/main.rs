//! # Macros Demo
//!
//! Demonstrates Ranvier procedural macros that reduce boilerplate when
//! building Axon circuits.
//!
//! ## Run
//! ```bash
//! cargo run -p macros-demo
//! ```
//!
//! ## Key Concepts
//! - `#[transition]` — derive a `Transition` impl from an async function
//! - `bus_allow` / `bus_deny` — compile-time Bus access control
//!
//! ## Before / After Comparison
//!
//! **Without macros** (manual Transition impl):
//! ```rust,ignore
//! #[derive(Clone)]
//! struct Validate;
//!
//! #[async_trait]
//! impl Transition<OrderRequest, ValidatedOrder> for Validate {
//!     type Error = String;
//!     type Resources = ();
//!
//!     async fn run(
//!         &self,
//!         input: OrderRequest,
//!         _resources: &Self::Resources,
//!         _bus: &mut Bus,
//!     ) -> Outcome<ValidatedOrder, String> {
//!         // ... validation logic ...
//!     }
//! }
//! ```
//!
//! **With `#[transition]` macro** (same behavior, less boilerplate):
//! ```rust,ignore
//! #[transition]
//! async fn validate(input: OrderRequest, _res: &(), _bus: &mut Bus)
//!     -> Outcome<ValidatedOrder, String>
//! {
//!     // ... validation logic ...
//! }
//! ```

use ranvier_core::bus::Bus;
use ranvier_core::outcome::Outcome;
use ranvier_macros::transition;
use ranvier_runtime::Axon;
use serde::{Deserialize, Serialize};

// ── Domain types ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OrderRequest {
    order_id: String,
    customer: String,
    amount: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ValidatedOrder {
    order_id: String,
    customer: String,
    amount: f64,
    validated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProcessedOrder {
    order_id: String,
    status: String,
    total: f64,
}

// ── Transitions using #[transition] macro ─────────────────────────────────

/// Validates incoming orders.
/// The macro generates a `validate` struct that implements `Transition<OrderRequest, ValidatedOrder>`.
#[transition]
async fn validate(input: OrderRequest, _res: &(), _bus: &mut Bus) -> Outcome<ValidatedOrder, String> {
    if input.amount <= 0.0 {
        return Outcome::fault(format!("Invalid amount: {}", input.amount));
    }
    Outcome::next(ValidatedOrder {
        order_id: input.order_id,
        customer: input.customer,
        amount: input.amount,
        validated: true,
    })
}

/// Processes validated orders — applies tax and finalizes.
/// The macro infers Resources = () and Error = String from the signature.
#[transition]
async fn process(input: ValidatedOrder, _res: &(), _bus: &mut Bus) -> Outcome<ProcessedOrder, String> {
    let tax = input.amount * 0.1;
    Outcome::next(ProcessedOrder {
        order_id: input.order_id,
        status: "completed".to_string(),
        total: input.amount + tax,
    })
}

// ── Bus access control with bus_allow / bus_deny ──────────────────────────

/// Only allowed to read `f64` (discount rate) from Bus.
#[transition(bus_allow = [f64])]
async fn apply_discount(
    input: ValidatedOrder,
    _res: &(),
    bus: &mut Bus,
) -> Outcome<ValidatedOrder, String> {
    let discount = bus.get::<f64>().map(|d| *d).unwrap_or(0.0);
    Outcome::next(ValidatedOrder {
        amount: input.amount * (1.0 - discount),
        ..input
    })
}

/// Denied from accessing `String` (secrets) in Bus.
#[transition(bus_deny = [String])]
async fn finalize(input: ProcessedOrder, _res: &(), _bus: &mut Bus) -> Outcome<ProcessedOrder, String> {
    Outcome::next(ProcessedOrder {
        status: format!("{} (finalized)", input.status),
        ..input
    })
}

// ── Main ──────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    println!("=== Ranvier Macros Demo ===");
    println!();

    // ── 1. Basic #[transition] pipeline ───────────────────────────
    println!("--- Basic Pipeline (validate -> process) ---");

    let pipeline = Axon::<OrderRequest, OrderRequest, String>::new("BasicPipeline")
        .then(validate)
        .then(process);

    let order = OrderRequest {
        order_id: "ORD-100".to_string(),
        customer: "Alice".to_string(),
        amount: 200.0,
    };

    let mut bus = Bus::new();
    let result = pipeline.execute(order.clone(), &(), &mut bus).await;
    println!("  Input:  {:?}", order);
    println!("  Output: {:?}", result);
    println!();

    // ── 2. Pipeline with bus_allow discount ───────────────────────
    println!("--- Pipeline with Bus Discount (bus_allow = [f64]) ---");

    let discount_pipeline = Axon::<OrderRequest, OrderRequest, String>::new("DiscountPipeline")
        .then(validate)
        .then(apply_discount)
        .then(process);

    let order2 = OrderRequest {
        order_id: "ORD-200".to_string(),
        customer: "Bob".to_string(),
        amount: 500.0,
    };

    let mut bus2 = Bus::new();
    bus2.insert(0.15_f64); // 15% discount

    let result2 = discount_pipeline.execute(order2.clone(), &(), &mut bus2).await;
    println!("  Input:    {:?}", order2);
    println!("  Discount: 15%");
    println!("  Output:   {:?}", result2);
    println!();

    // ── 3. Pipeline with bus_deny ─────────────────────────────────
    println!("--- Pipeline with bus_deny = [String] ---");

    let secure_pipeline = Axon::<OrderRequest, OrderRequest, String>::new("SecurePipeline")
        .then(validate)
        .then(process)
        .then(finalize);

    let order3 = OrderRequest {
        order_id: "ORD-300".to_string(),
        customer: "Charlie".to_string(),
        amount: 1000.0,
    };

    let mut bus3 = Bus::new();
    bus3.insert("secret-api-key".to_string()); // Present but denied to `finalize`

    let result3 = secure_pipeline.execute(order3.clone(), &(), &mut bus3).await;
    println!("  Input:  {:?}", order3);
    println!("  Output: {:?}", result3);
    println!();

    // ── 4. Error handling ─────────────────────────────────────────
    println!("--- Error Handling ---");

    let bad_order = OrderRequest {
        order_id: "ORD-400".to_string(),
        customer: "Dave".to_string(),
        amount: -50.0,
    };

    let mut bus4 = Bus::new();
    let result4 = pipeline.execute(bad_order.clone(), &(), &mut bus4).await;
    println!("  Input:  {:?}", bad_order);
    println!("  Output: {:?}", result4);

    println!();
    println!("=== Demo complete ===");
}
