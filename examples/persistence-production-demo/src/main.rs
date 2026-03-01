//! persistence-production-demo (M156: Persistence Production Gate)
//!
//! Demonstrates three production-grade persistence scenarios using
//! `InMemoryPersistenceStore` (no external dependencies required):
//!
//! **Scenario 1: Checkpoint and crash recovery**
//!   - Execute steps 0..N, saving a checkpoint after each
//!   - Simulate a crash after step 1
//!   - Resume from the last checkpoint
//!
//! **Scenario 2: Compensation hook on fault**
//!   - Execute a workflow that faults at step 2
//!   - Trigger compensation hook to reverse side effects
//!   - Verify CompletionState::Compensated is recorded
//!
//! **Scenario 3: Idempotent compensation (duplicate prevention)**
//!   - Attempt to compensate the same trace_id twice
//!   - Idempotency store prevents the second execution
//!
//! See `docs/03_guides/persistence_ops_runbook.md` for operational guidance.

use anyhow::Result;
use async_trait::async_trait;
use ranvier_runtime::persistence::*;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

// ── Helpers ────────────────────────────────────────────────────────────────────

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn envelope(trace_id: &str, circuit: &str, step: u64, kind: &str) -> PersistenceEnvelope {
    PersistenceEnvelope {
        trace_id: trace_id.to_string(),
        circuit: circuit.to_string(),
        step,
        outcome_kind: kind.to_string(),
        timestamp_ms: now_ms(),
        payload_hash: None,
    }
}

// ── Scenario 1: Checkpoint + crash recovery ────────────────────────────────────

async fn scenario_checkpoint_recovery() -> Result<()> {
    println!("──────────────────────────────────────────");
    println!("Scenario 1: Checkpoint and Crash Recovery");
    println!("──────────────────────────────────────────");

    let store = InMemoryPersistenceStore::new();
    let trace_id = "order-001";
    let circuit = "order-pipeline";

    // Simulate executing steps 0 and 1 successfully
    store.append(envelope(trace_id, circuit, 0, "Next")).await?;
    println!("[step 0] checkpoint saved — validate_order");

    store.append(envelope(trace_id, circuit, 1, "Next")).await?;
    println!("[step 1] checkpoint saved — charge_payment");

    // Simulate process crash — step 2 never executes
    println!("[CRASH]  simulating process crash before step 2");

    // ─── After restart ───────────────────────────────────────────────────────
    // On restart: load the interrupted trace and find the last completed step
    let persisted = store.load(trace_id).await?;
    assert!(persisted.is_some(), "trace should be found after restart");
    let trace = persisted.unwrap();

    assert!(
        trace.completion.is_none(),
        "completion should be None — workflow was interrupted"
    );

    let last_step = trace.events.last().map(|e| e.step).unwrap_or(0);
    let cursor = store.resume(trace_id, last_step).await?;

    println!(
        "[RESUME] resuming from step {}, next_step = {}",
        last_step, cursor.next_step
    );

    // Resume: execute remaining steps
    store.append(envelope(trace_id, circuit, cursor.next_step, "Next")).await?;
    println!("[step {}] checkpoint saved — send_confirmation", cursor.next_step);

    store.complete(trace_id, CompletionState::Success).await?;

    let final_trace = store.load(trace_id).await?.unwrap();
    assert_eq!(final_trace.completion, Some(CompletionState::Success));
    println!("[DONE]   workflow completed successfully");
    println!("         total_steps = {}", final_trace.events.len());
    println!();

    Ok(())
}

// ── Scenario 2: Compensation hook ─────────────────────────────────────────────

/// Example compensation hook — records compensations for verification
struct RecordingCompensationHook {
    log: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl CompensationHook for RecordingCompensationHook {
    async fn compensate(&self, ctx: CompensationContext) -> Result<()> {
        let msg = format!(
            "refund trace={} fault_at_step={}",
            ctx.trace_id, ctx.fault_step
        );
        tracing::warn!(
            trace_id = %ctx.trace_id,
            fault_step = ctx.fault_step,
            "compensation triggered — issuing refund"
        );
        self.log.lock().unwrap().push(msg);
        Ok(())
    }
}

async fn scenario_compensation_hook() -> Result<()> {
    println!("──────────────────────────────────────");
    println!("Scenario 2: Compensation Hook on Fault");
    println!("──────────────────────────────────────");

    let store = InMemoryPersistenceStore::new();
    let trace_id = "order-002";
    let circuit = "order-pipeline";

    let compensation_log = Arc::new(Mutex::new(Vec::<String>::new()));
    let hook = RecordingCompensationHook { log: compensation_log.clone() };

    // Steps 0 and 1 succeed
    store.append(envelope(trace_id, circuit, 0, "Next")).await?;
    println!("[step 0] validate_order — OK");

    store.append(envelope(trace_id, circuit, 1, "Next")).await?;
    println!("[step 1] charge_payment — OK (external side effect committed!)");

    // Step 2 faults (e.g., inventory unavailable)
    store.append(envelope(trace_id, circuit, 2, "Fault")).await?;
    println!("[step 2] reserve_inventory — FAULT");

    // Trigger compensation
    println!("[COMPENSATE] running compensation for committed side effects...");
    let ctx = CompensationContext {
        trace_id: trace_id.to_string(),
        circuit: circuit.to_string(),
        fault_kind: "inventory_unavailable".to_string(),
        fault_step: 2,
        timestamp_ms: now_ms(),
    };
    hook.compensate(ctx).await?;

    store.complete(trace_id, CompletionState::Compensated).await?;

    let final_trace = store.load(trace_id).await?.unwrap();
    assert_eq!(final_trace.completion, Some(CompletionState::Compensated));

    let log = compensation_log.lock().unwrap();
    println!("[DONE]   compensation complete: {:?}", *log);
    println!();

    Ok(())
}

// ── Scenario 3: Idempotent compensation ───────────────────────────────────────

async fn scenario_idempotent_compensation() -> Result<()> {
    println!("────────────────────────────────────────────────");
    println!("Scenario 3: Idempotent Compensation (no duplicates)");
    println!("────────────────────────────────────────────────");

    let idempotency = InMemoryCompensationIdempotencyStore::new();
    let compensation_count = Arc::new(Mutex::new(0u32));

    let idempotency_key = "order-003:refund";

    for attempt in 1u32..=3 {
        let already_done = idempotency.was_compensated(idempotency_key).await?;
        if already_done {
            println!("[attempt {}] compensation already done — skipping (idempotent)", attempt);
            continue;
        }

        // Execute compensation exactly once
        *compensation_count.lock().unwrap() += 1;
        println!("[attempt {}] running compensation...", attempt);
        // ... actual refund logic here

        idempotency.mark_compensated(idempotency_key).await?;
        println!("[attempt {}] compensation recorded", attempt);
    }

    let count = *compensation_count.lock().unwrap();
    assert_eq!(count, 1, "compensation should have run exactly once");
    println!("[DONE]   compensation ran {} time(s) — correct!", count);
    println!();

    Ok(())
}

// ── Main ───────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("warn".parse()?)
        )
        .init();

    println!("=== Persistence Production Demo (M156) ===");
    println!();

    scenario_checkpoint_recovery().await?;
    scenario_compensation_hook().await?;
    scenario_idempotent_compensation().await?;

    println!("All scenarios passed. See docs/03_guides/persistence_ops_runbook.md");
    println!("for production deployment guidance.");

    Ok(())
}
