//! State Persistence & Recovery Demo
//!
//! ## Purpose
//! Demonstrates process-local persistence observations for workflow execution:
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
    InMemoryPersistenceStore, PersistenceAutoComplete, PersistenceEventType, PersistenceHandle,
    PersistenceStore, PersistenceTraceId,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

fn recorded_outcome<T>(
    report: ranvier_runtime::ExecutionReport<Outcome<T, String>>,
    complete: bool,
) -> anyhow::Result<Outcome<T, String>> {
    if report
        .cleanup()
        .is_some_and(|cleanup| !cleanup.is_complete())
    {
        anyhow::bail!("Cleanup requires reconciliation");
    }
    match report {
        ranvier_runtime::ExecutionReport::Recorded {
            result, completion, ..
        } if completion
            == if complete {
                ranvier_runtime::CompletionConfirmation::Confirmed
            } else {
                ranvier_runtime::CompletionConfirmation::NotRequested
            } =>
        {
            Ok(result)
        }
        _ => anyhow::bail!("Persistence report requires reconciliation before retry"),
    }
}

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

fn verify_trace(
    trace: &ranvier_runtime::PersistedTrace,
    id: &str,
    completion: Option<ranvier_runtime::CompletionState>,
) -> anyhow::Result<()> {
    if trace.trace_id != id
        || trace.circuit != "order.pipeline"
        || trace.completion != completion
        || trace.events.is_empty()
    {
        anyhow::bail!("Stored trace identity or completion mismatch");
    }
    let invalid_event = trace.events.iter().any(|event| {
        event.trace_id != id
            || event.circuit != trace.circuit
            || match event.event_type {
                PersistenceEventType::Intent
                | PersistenceEventType::Result
                | PersistenceEventType::Compensation
                | PersistenceEventType::Checkpoint => event.node_id.is_none(),
                PersistenceEventType::Terminal => event.node_id.is_some(),
                PersistenceEventType::Enter => event.outcome_kind != "Enter",
                PersistenceEventType::LegacyUnknown => true,
            }
    });
    let invalid_sequence = trace
        .events
        .iter()
        .enumerate()
        .any(|(expected, event)| event.event_sequence != expected as u64);
    if invalid_event || invalid_sequence {
        anyhow::bail!(
            "Stored event identity/order mismatch: {:?}",
            trace
                .events
                .iter()
                .map(|event| (
                    event.event_sequence,
                    event.event_type,
                    event.step,
                    event.outcome_kind.as_str(),
                    event.node_id.is_some()
                ))
                .collect::<Vec<_>>()
        );
    }
    Ok(())
}

async fn seed_interrupted_checkpoint(
    store: &InMemoryPersistenceStore,
    axon: &Axon<OrderState, String, String>,
    trace_id: &str,
    input: &OrderState,
    checkpoint: &OrderState,
) -> anyhow::Result<()> {
    let schematic = axon.schematic();
    let entry_node_id = schematic
        .nodes
        .first()
        .map(|node| node.id.to_string())
        .ok_or_else(|| anyhow::anyhow!("entry node missing"))?;
    let validate_node_id = schematic
        .nodes
        .get(1)
        .map(|node| node.id.to_string())
        .ok_or_else(|| anyhow::anyhow!("validation node missing"))?;
    let envelope = |event_sequence, event_type, step, node_id, outcome_kind: &str, payload| {
        ranvier_runtime::PersistenceEnvelope {
            trace_id: trace_id.to_owned(),
            circuit: schematic.name.clone(),
            schematic_version: schematic.schema_version.clone(),
            event_sequence,
            event_type,
            step,
            node_id,
            outcome_kind: outcome_kind.to_owned(),
            timestamp_ms: 0,
            payload_hash: None,
            payload,
        }
    };
    for event in [
        envelope(
            0,
            PersistenceEventType::Enter,
            0,
            Some(entry_node_id),
            "Enter",
            None,
        ),
        envelope(
            1,
            PersistenceEventType::Intent,
            1,
            Some(validate_node_id.clone()),
            "Intent",
            Some(serde_json::to_value(input)?),
        ),
        envelope(
            2,
            PersistenceEventType::Result,
            1,
            Some(validate_node_id),
            "Next",
            Some(serde_json::to_value(checkpoint)?),
        ),
    ] {
        store.append(event).await?;
    }
    Ok(())
}

// ============================================================================
// Main
// ============================================================================

async fn run_demo() -> anyhow::Result<()> {
    println!("=== State Persistence & Recovery Demo ===\n");

    let store_impl = Arc::new(InMemoryPersistenceStore::new());
    let store_dyn: Arc<dyn PersistenceStore> = store_impl.clone();
    let handle = PersistenceHandle::from_arc(store_dyn);
    let axon = build_order_axon();

    // --- Phase 1: Simulate process stop after a settled validation checkpoint ---
    println!("--- Phase 1: Process stops after the validation checkpoint ---");

    let trace_id = "order-5001";
    let input = OrderState {
        order_id: "5001".into(),
        validated: false,
        should_fail_payment: false,
    };
    let checkpoint = OrderState {
        validated: true,
        ..input.clone()
    };
    seed_interrupted_checkpoint(&store_impl, &axon, trace_id, &input, &checkpoint).await?;
    println!("  Persisted validation checkpoint; payment step has not run");

    let trace = store_impl
        .load(trace_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("missing trace"))?;
    verify_trace(&trace, trace_id, None)?;
    if trace
        .events
        .last()
        .is_none_or(|event| event.event_type != PersistenceEventType::Result)
    {
        anyhow::bail!("Settled validation checkpoint missing");
    }

    // --- Phase 2: Resume after the last settled checkpoint and complete ---
    println!("\n--- Phase 2: Resume from checkpoint and complete ---");

    // Resume only from a settled nonterminal result, never from a terminal fault.
    let resume_from = trace
        .events
        .iter()
        .rev()
        .find(|event| {
            event.event_type == PersistenceEventType::Result && event.outcome_kind == "Next"
        })
        .map(|event| event.step)
        .ok_or_else(|| {
            anyhow::anyhow!("Successful checkpoint missing; operator reconciliation required")
        })?;
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

    let outcome = recorded_outcome(axon.execute(input, &(), &mut bus).await, true)?;
    if !matches!(&outcome, Outcome::Next(value) if value == "TRACK-5001") {
        anyhow::bail!("Checkpoint resume did not complete expected order");
    }
    match &outcome {
        Outcome::Next(tracking) => println!("  Outcome: SUCCESS — {}", tracking),
        Outcome::Fault(e) => println!("  Outcome: FAULT — {}", e),
        _ => {}
    }

    let trace = store_impl
        .load(trace_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("missing trace"))?;
    verify_trace(
        &trace,
        trace_id,
        Some(ranvier_runtime::CompletionState::Success),
    )?;
    if trace.resumed_from_step != Some(resume_from)
        || trace
            .events
            .last()
            .is_none_or(|event| event.outcome_kind != "Next")
    {
        anyhow::bail!("Resume observation mismatch");
    }

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

    let outcome = recorded_outcome(axon.execute(input, &(), &mut bus).await, true)?;
    if !matches!(&outcome, Outcome::Fault(error) if error == "payment_declined") {
        anyhow::bail!("Compensation scenario fault missing");
    }
    match &outcome {
        Outcome::Fault(e) => println!("  Outcome: FAULT — {} (compensation executed)", e),
        _ => println!("  Outcome: unexpected"),
    }

    let trace = store_impl
        .load(comp_trace_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("missing trace"))?;
    verify_trace(
        &trace,
        comp_trace_id,
        Some(ranvier_runtime::CompletionState::Compensated),
    )?;

    println!("\ndone");
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    run_demo().await
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn actual_persistence_phases() {
        run_demo().await.unwrap();
    }
}
