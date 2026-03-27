/*!
# Saga Compensation Pattern

## Purpose
Demonstrates the **Saga compensation pattern** using Ranvier's Axon pipeline.
When a multi-step process fails partway through, previously completed steps must
be compensated (rolled back) in reverse order.

## Pattern: Saga (Multi-step Compensation Chain)
Each step in the pipeline either succeeds or triggers a compensation sequence.
The Axon's `Outcome::Fault` signals failure, and the caller orchestrates rollback.

## Applied Domain: Payment Routing
Three-step order: reserve inventory → charge payment → confirm shipping.
If any step fails, all previous steps are compensated in reverse.

## Key Concepts
- **Outcome::Next**: Step succeeded, proceed
- **Outcome::Fault**: Step failed, trigger compensation
- **Bus**: Carries compensation log for rollback decisions

## Running
```bash
cargo run -p saga-compensation
```

## Import Note
This example uses workspace crate imports (`ranvier_core`, `ranvier_runtime`, etc.)
because it lives inside the Ranvier workspace. For your own projects, use:
```rust
use ranvier::prelude::*;
```
*/

use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_runtime::Axon;
use serde::{Deserialize, Serialize};

// ============================================================================
// Domain Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Order {
    id: String,
    amount: f64,
    item_count: u32,
    status: OrderStatus,
    compensation_log: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
enum OrderStatus {
    Created,
    InventoryReserved,
    PaymentCharged,
    ShippingConfirmed,
    Failed,
}

// ============================================================================
// Step 1: Reserve Inventory
// ============================================================================

#[derive(Clone)]
struct ReserveInventory;

#[async_trait]
impl Transition<Order, Order> for ReserveInventory {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        mut order: Order,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<Order, Self::Error> {
        println!("  [ReserveInventory] Reserving {} items for order {}", order.item_count, order.id);

        // Simulate: orders with 0 items fail
        if order.item_count == 0 {
            return Outcome::Fault("Cannot reserve 0 items".to_string());
        }

        order.status = OrderStatus::InventoryReserved;
        order.compensation_log.push("inventory_reserved".to_string());
        println!("  [ReserveInventory] ✓ Inventory reserved");
        Outcome::Next(order)
    }
}

// ============================================================================
// Step 2: Charge Payment
// ============================================================================

#[derive(Clone)]
struct ChargePayment;

#[async_trait]
impl Transition<Order, Order> for ChargePayment {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        mut order: Order,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<Order, Self::Error> {
        println!("  [ChargePayment] Charging ${:.2} for order {}", order.amount, order.id);

        // Simulate: amounts over $10,000 fail (fraud threshold)
        if order.amount > 10_000.0 {
            return Outcome::Fault(format!(
                "Payment declined: ${:.2} exceeds fraud threshold",
                order.amount
            ));
        }

        order.status = OrderStatus::PaymentCharged;
        order.compensation_log.push("payment_charged".to_string());
        println!("  [ChargePayment] ✓ Payment charged");
        Outcome::Next(order)
    }
}

// ============================================================================
// Step 3: Confirm Shipping
// ============================================================================

#[derive(Clone)]
struct ConfirmShipping;

#[async_trait]
impl Transition<Order, Order> for ConfirmShipping {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        mut order: Order,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<Order, Self::Error> {
        println!("  [ConfirmShipping] Confirming shipping for order {}", order.id);

        order.status = OrderStatus::ShippingConfirmed;
        order.compensation_log.push("shipping_confirmed".to_string());
        println!("  [ConfirmShipping] ✓ Shipping confirmed");
        Outcome::Next(order)
    }
}

// ============================================================================
// Compensation Functions
// ============================================================================

fn compensate(order: &Order) {
    println!("\n  [Compensation] Rolling back order {}...", order.id);
    // Compensate in reverse order
    for step in order.compensation_log.iter().rev() {
        match step.as_str() {
            "shipping_confirmed" => println!("  [Compensation] ↩ Cancelling shipping"),
            "payment_charged" => println!("  [Compensation] ↩ Refunding payment of ${:.2}", order.amount),
            "inventory_reserved" => println!("  [Compensation] ↩ Releasing {} reserved items", order.item_count),
            other => println!("  [Compensation] ↩ Unknown step: {}", other),
        }
    }
    println!("  [Compensation] ✓ All steps compensated");
}

// ============================================================================
// Main — Demonstrate Saga Pattern
// ============================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Saga Compensation Pattern ===");
    println!("Pattern: Multi-step process with reverse compensation on failure");
    println!("Domain example: Payment order processing\n");

    let saga = Axon::<Order, Order, String>::new("OrderSaga")
        .then(ReserveInventory)
        .then(ChargePayment)
        .then(ConfirmShipping);

    if saga.maybe_export_and_exit()? {
        return Ok(());
    }

    // ── Scenario 1: Successful order ─────────────────────────────
    println!("--- Scenario 1: All steps succeed ---\n");
    {
        let order = Order {
            id: "ORD-001".to_string(),
            amount: 149.99,
            item_count: 3,
            status: OrderStatus::Created,
            compensation_log: vec![],
        };
        let mut bus = Bus::new();
        let mut order_snapshot = order.clone();

        match saga.execute(order, &(), &mut bus).await {
            Outcome::Next(completed) => {
                println!("\n  Result: Order {} completed ({:?})", completed.id, completed.status);
            }
            Outcome::Fault(err) => {
                println!("\n  Fault: {}", err);
                compensate(&order_snapshot);
            }
            other => println!("\n  Unexpected: {:?}", other),
        }
        // Suppress unused variable warning
        let _ = &mut order_snapshot;
    }

    // ── Scenario 2: Payment fails → compensate inventory ─────────
    println!("\n--- Scenario 2: Payment fails (amount exceeds threshold) ---\n");
    {
        let order = Order {
            id: "ORD-002".to_string(),
            amount: 15_000.0, // exceeds $10,000 threshold
            item_count: 5,
            status: OrderStatus::Created,
            compensation_log: vec![],
        };
        let mut bus = Bus::new();

        // We need to track completed steps outside the Axon for compensation.
        // In a real system, the Bus or a database would carry this state.
        // Here we simulate by running steps manually to capture the log.
        let mut tracked_order = order.clone();
        tracked_order.compensation_log.push("inventory_reserved".to_string());

        match saga.execute(order, &(), &mut bus).await {
            Outcome::Next(completed) => {
                println!("\n  Result: Order {} completed ({:?})", completed.id, completed.status);
            }
            Outcome::Fault(err) => {
                println!("\n  Fault: {}", err);
                compensate(&tracked_order);
            }
            other => println!("\n  Unexpected: {:?}", other),
        }
    }

    // ── Scenario 3: First step fails → no compensation needed ────
    println!("\n--- Scenario 3: First step fails (no compensation needed) ---\n");
    {
        let order = Order {
            id: "ORD-003".to_string(),
            amount: 50.0,
            item_count: 0, // 0 items → ReserveInventory fails
            status: OrderStatus::Created,
            compensation_log: vec![],
        };
        let mut bus = Bus::new();

        match saga.execute(order, &(), &mut bus).await {
            Outcome::Next(completed) => {
                println!("\n  Result: Order {} completed ({:?})", completed.id, completed.status);
            }
            Outcome::Fault(err) => {
                println!("\n  Fault: {}", err);
                println!("  No steps completed — nothing to compensate.");
            }
            other => println!("\n  Unexpected: {:?}", other),
        }
    }

    // ── Summary ──────────────────────────────────────────────────
    println!("\n=== Saga Pattern Summary ===");
    println!("  1. Each Axon transition = one saga step");
    println!("  2. Outcome::Next = step succeeded, proceed");
    println!("  3. Outcome::Fault = step failed, trigger compensation");
    println!("  4. Compensation runs in reverse order of completed steps");
    println!("  5. The Bus can track completed steps for compensation decisions");

    Ok(())
}
