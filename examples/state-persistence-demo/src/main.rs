//! State Persistence & Recovery Demo
//!
//! ## Purpose
//! Demonstrates Ranvier's persistence layer for durable workflow execution:
//! fault recovery via checkpointed traces and compensation hooks for rollback.
//!
//! ## Run
//! ```bash
//! cargo run -p state-persistence-demo
//! ```
//!
//! ## Key Concepts
//! - `InMemoryPersistenceStore` for trace storage
//! - `PersistenceHandle` + `PersistenceTraceId` for trace tracking
//! - Fault → persist → resume workflow pattern
//! - `CompensationHook` for rollback on irrecoverable faults
//! - `ranvier_bus!` macro for Bus capability injection
//!
//! ## Prerequisites
//! - `hello-world` — basic Transition + Axon usage
//! - `retry-dlq-demo` — retry and DLQ patterns
//!
//! ## Production Storage
//! This demo uses `InMemoryPersistenceStore` for simplicity. For production,
//! enable a durable backend:
//! ```toml
//! # Cargo.toml
//! ranvier-runtime = { version = "0.32", features = ["persistence-postgres"] }
//! ```
//! ```rust,ignore
//! use ranvier_runtime::PostgresPersistenceStore;
//! let store = PostgresPersistenceStore::new(pg_pool);
//! store.ensure_schema().await?;
//! ```
//! See also: `persistence-redis` feature for ephemeral/fast checkpoints.
//!
//! ## Next Steps
//! - `multitenancy-demo` — tenant isolation patterns
//! - `order-processing-demo` — production-style multi-step workflow

use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_runtime::{
    Axon, CompensationContext, CompensationHandle, CompensationHook, CompensationRetryPolicy,
    InMemoryPersistenceStore, PersistenceAutoComplete, PersistenceHandle, PersistenceStore,
    PersistenceTraceId,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// ============================================================================
// Domain Types
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
struct OrderState {
    order_id: String,
    validated: bool,
    should_fail_payment: bool,
}

// ============================================================================
// Transitions
// ============================================================================

#[derive(Clone)]
struct ValidateOrder;

#[async_trait]
impl Transition<OrderState, OrderState> for ValidateOrder {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        mut state: OrderState,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<OrderState, Self::Error> {
        println!("  [ValidateOrder] Validating order {}", state.order_id);
        state.validated = true;
        Outcome::Next(state)
    }
}

#[derive(Clone)]
struct ChargePayment;

#[async_trait]
impl Transition<OrderState, OrderState> for ChargePayment {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        state: OrderState,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<OrderState, Self::Error> {
        if state.should_fail_payment {
            println!("  [ChargePayment] Payment DECLINED for {}", state.order_id);
            return Outcome::Fault("payment_declined".to_string());
        }
        println!("  [ChargePayment] Payment charged for {}", state.order_id);
        Outcome::Next(state)
    }
}

#[derive(Clone)]
struct FinalizeOrder;

#[async_trait]
impl Transition<OrderState, String> for FinalizeOrder {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        state: OrderState,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        let tracking = format!("TRACK-{}", state.order_id);
        println!("  [FinalizeOrder] Order finalized: {}", tracking);
        Outcome::Next(tracking)
    }
}

// ============================================================================
// Compensation Hook
// ============================================================================

#[derive(Clone)]
struct RefundCompensation;

#[async_trait]
impl CompensationHook for RefundCompensation {
    async fn compensate(&self, context: CompensationContext) -> anyhow::Result<()> {
        println!(
            "  [Compensation] Refunding trace={} circuit={} fault_step={} reason={}",
            context.trace_id, context.circuit, context.fault_step, context.fault_kind
        );
        Ok(())
    }
}

// ============================================================================
// Helpers
// ============================================================================

fn build_order_axon() -> Axon<OrderState, String, String> {
    Axon::<OrderState, OrderState, String>::new("order.pipeline")
        .then(ValidateOrder)
        .then(ChargePayment)
        .then(FinalizeOrder)
}

fn print_trace(label: &str, trace: &ranvier_runtime::PersistedTrace) {
    println!(
        "  [Trace:{}] id={} circuit={}",
        label, trace.trace_id, trace.circuit
    );
    println!("    events: {}", trace.events.len());
    for event in &trace.events {
        println!(
            "      step={} outcome={} ts={}",
            event.step, event.outcome_kind, event.timestamp_ms
        );
    }
    println!("    resumed_from_step: {:?}", trace.resumed_from_step);
    println!("    completion: {:?}", trace.completion);
}

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== State Persistence & Recovery Demo ===\n");

    let store_impl = Arc::new(InMemoryPersistenceStore::new());
    let store_dyn: Arc<dyn PersistenceStore> = store_impl.clone();
    let handle = PersistenceHandle::from_arc(store_dyn);
    let axon = build_order_axon();

    // --- Phase 1: First run faults at payment (trace kept open) ---
    println!("--- Phase 1: First run — payment fault (trace stays open) ---");

    let trace_id = "order-5001";
    let mut bus = ranvier_core::ranvier_bus!(
        handle.clone(),
        PersistenceTraceId::new(trace_id),
        PersistenceAutoComplete(false),
    );

    let input = OrderState {
        order_id: "5001".into(),
        validated: false,
        should_fail_payment: true,
    };

    let outcome = axon.execute(input, &(), &mut bus).await;
    match &outcome {
        Outcome::Fault(e) => println!("  Outcome: FAULT — {}", e),
        Outcome::Next(t) => println!("  Outcome: SUCCESS — {}", t),
        _ => {}
    }

    let trace = store_impl
        .load(trace_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("missing trace"))?;
    print_trace("after phase 1", &trace);

    // --- Phase 2: Resume from checkpoint (fix condition, complete) ---
    println!("\n--- Phase 2: Resume — fix payment condition and complete ---");

    // Resume from the latest successful checkpoint, not from a terminal fault marker.
    let resume_from = trace
        .events
        .iter()
        .rev()
        .find(|event| event.outcome_kind == "Next")
        .or_else(|| trace.events.last())
        .map(|event| event.step)
        .unwrap_or(0);
    let cursor = store_impl.resume(trace_id, resume_from).await?;
    println!(
        "  Resume cursor: trace={} next_step={}",
        cursor.trace_id, cursor.next_step
    );

    let mut bus = ranvier_core::ranvier_bus!(
        handle.clone(),
        PersistenceTraceId::new(trace_id),
        PersistenceAutoComplete(true),
    );

    let input = OrderState {
        order_id: "5001".into(),
        validated: false,
        should_fail_payment: false,
    };

    let outcome = axon.execute(input, &(), &mut bus).await;
    match &outcome {
        Outcome::Next(tracking) => println!("  Outcome: SUCCESS — {}", tracking),
        Outcome::Fault(e) => println!("  Outcome: FAULT — {}", e),
        _ => {}
    }

    let trace = store_impl
        .load(trace_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("missing trace"))?;
    print_trace("after phase 2", &trace);

    // --- Phase 3: Compensation hook on irrecoverable fault ---
    println!("\n--- Phase 3: Compensation hook on irrecoverable fault ---");

    let comp_trace_id = "order-6001";
    let mut bus = ranvier_core::ranvier_bus!(
        handle,
        PersistenceTraceId::new(comp_trace_id),
        CompensationHandle::from_hook(RefundCompensation),
        CompensationRetryPolicy {
            max_attempts: 1,
            backoff_ms: 0,
        },
    );

    let input = OrderState {
        order_id: "6001".into(),
        validated: false,
        should_fail_payment: true,
    };

    let outcome = axon.execute(input, &(), &mut bus).await;
    match &outcome {
        Outcome::Fault(e) => println!("  Outcome: FAULT — {} (compensation executed)", e),
        _ => println!("  Outcome: unexpected"),
    }

    let trace = store_impl
        .load(comp_trace_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("missing trace"))?;
    print_trace("after compensation", &trace);

    println!("\ndone");
    Ok(())
}
