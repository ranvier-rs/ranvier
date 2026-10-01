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
//! - `bus_allow` / `bus_deny` — compile-time declaration checks with runtime Bus enforcement
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

fn local_demo_outcome<T, E>(
    report: ranvier_runtime::ExecutionReport<Outcome<T, E>>,
) -> Result<Outcome<T, E>, String> {
    if report
        .cleanup()
        .is_some_and(|cleanup| !cleanup.is_complete())
    {
        return Err("Execution requires reconciliation before retry".into());
    }
    match report {
        ranvier_runtime::ExecutionReport::Unpersisted { result, .. } => Ok(result),
        _ => Err("Execution requires reconciliation before retry".into()),
    }
}

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
async fn validate(
    input: OrderRequest,
    _res: &(),
    _bus: &mut Bus,
) -> Outcome<ValidatedOrder, String> {
    if !input.amount.is_finite() || input.amount <= 0.0 {
        return Outcome::fault("Invalid amount".into());
    }
    if input.order_id.trim().is_empty()
        || input.order_id.len() > 128
        || input.customer.trim().is_empty()
        || input.customer.len() > 128
    {
        return Outcome::fault("Invalid order identity".into());
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
async fn process(
    input: ValidatedOrder,
    _res: &(),
    _bus: &mut Bus,
) -> Outcome<ProcessedOrder, String> {
    if !input.validated || !input.amount.is_finite() || input.amount < 0.0 {
        return Outcome::fault("Invalid validated order".into());
    }
    let tax = input.amount * 0.1;
    if !(input.amount + tax).is_finite() {
        return Outcome::fault("Amount overflow".into());
    }
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
    if !input.validated || !input.amount.is_finite() || input.amount < 0.0 {
        return Outcome::fault("Invalid validated order".into());
    }
    let discount = match bus.get::<f64>() {
        Ok(value) => *value,
        Err(ranvier_core::bus::BusAccessError::NotFound { .. }) => 0.0,
        Err(_) => return Outcome::fault("Discount capability denied".into()),
    };
    if !discount.is_finite() || !(0.0..=1.0).contains(&discount) {
        return Outcome::fault("Invalid discount".into());
    }
    Outcome::next(ValidatedOrder {
        amount: input.amount * (1.0 - discount),
        ..input
    })
}

/// Denied from accessing `String` (secrets) in Bus.
#[transition(bus_deny = [String])]
async fn finalize(
    input: ProcessedOrder,
    _res: &(),
    _bus: &mut Bus,
) -> Outcome<ProcessedOrder, String> {
    Outcome::next(ProcessedOrder {
        status: format!("{} (finalized)", input.status),
        ..input
    })
}

fn require_order(
    outcome: Outcome<ProcessedOrder, String>,
    total: f64,
    status: &str,
) -> Result<(), String> {
    match outcome {
        Outcome::Next(order) if order.total == total && order.status == status => Ok(()),
        _ => Err("Unexpected order outcome".into()),
    }
}

// ── Main ──────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), String> {
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
    let result = local_demo_outcome(pipeline.execute(order.clone(), &(), &mut bus).await)?;
    require_order(result, 220.0, "completed")?;
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

    let result2 = local_demo_outcome(
        discount_pipeline
            .execute(order2.clone(), &(), &mut bus2)
            .await,
    )?;
    println!("  Discount: 15%");
    require_order(result2, 467.5, "completed")?;
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

    let result3 = local_demo_outcome(
        secure_pipeline
            .execute(order3.clone(), &(), &mut bus3)
            .await,
    )?;
    require_order(result3, 1100.0, "completed (finalized)")?;
    println!();

    // ── 4. Error handling ─────────────────────────────────────────
    println!("--- Error Handling ---");

    let bad_order = OrderRequest {
        order_id: "ORD-400".to_string(),
        customer: "Dave".to_string(),
        amount: -50.0,
    };

    let mut bus4 = Bus::new();
    let result4 = local_demo_outcome(pipeline.execute(bad_order.clone(), &(), &mut bus4).await)?;
    if !matches!(result4, Outcome::Fault(ref error) if error == "Invalid amount") {
        return Err("Expected invalid amount rejection".into());
    }

    println!();
    println!("=== Demo complete ===");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ranvier_core::Transition;
    fn order(amount: f64) -> OrderRequest {
        OrderRequest {
            order_id: "id".into(),
            customer: "customer".into(),
            amount,
        }
    }
    #[tokio::test]
    async fn actual_macro_chain_rejects_invalid_amounts() {
        let flow = Axon::<OrderRequest, OrderRequest, String>::new("amount")
            .then(validate)
            .then(process);
        require_order(
            local_demo_outcome(flow.execute(order(200.0), &(), &mut Bus::new()).await).unwrap(),
            220.0,
            "completed",
        )
        .unwrap();
        for amount in [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            -1.0,
            0.0,
            f64::MAX,
        ] {
            assert!(matches!(
                local_demo_outcome(flow.execute(order(amount), &(), &mut Bus::new()).await)
                    .unwrap(),
                Outcome::Fault(_)
            ));
        }
        let mut blank = order(1.0);
        blank.customer.clear();
        assert!(matches!(
            local_demo_outcome(flow.execute(blank, &(), &mut Bus::new()).await).unwrap(),
            Outcome::Fault(_)
        ));
    }
    #[tokio::test]
    async fn discounts_and_finalization_are_verified() {
        let flow = Axon::<OrderRequest, OrderRequest, String>::new("discount")
            .then(validate)
            .then(apply_discount)
            .then(process)
            .then(finalize);
        for (discount, total) in [(None, 550.0), (Some(0.15), 467.5), (Some(1.0), 0.0)] {
            let mut bus = Bus::new();
            if let Some(rate) = discount {
                bus.insert(rate);
            }
            require_order(
                local_demo_outcome(flow.execute(order(500.0), &(), &mut bus).await).unwrap(),
                total,
                "completed (finalized)",
            )
            .unwrap();
        }
        for rate in [-0.1, 1.1, f64::NAN, f64::INFINITY] {
            let mut bus = Bus::new();
            bus.insert(rate);
            assert!(matches!(
                local_demo_outcome(flow.execute(order(1.0), &(), &mut bus).await).unwrap(),
                Outcome::Fault(_)
            ));
        }
        let forged = ValidatedOrder {
            order_id: "id".into(),
            customer: "customer".into(),
            amount: 1.0,
            validated: false,
        };
        assert!(matches!(
            process.run(forged.clone(), &(), &mut Bus::new()).await,
            Outcome::Fault(_)
        ));
        assert!(matches!(
            apply_discount.run(forged, &(), &mut Bus::new()).await,
            Outcome::Fault(_)
        ));
    }
}
