//! G0 regression tests against real PostgreSQL / Redis persistence stores.
//!
//! These run only when the matching URL is configured, like the adapter
//! round-trip tests in `persistence.rs`:
//!
//! - `RANVIER_PERSISTENCE_POSTGRES_URL` with `--features persistence-postgres`
//! - `RANVIER_PERSISTENCE_REDIS_URL` with `--features persistence-redis`
//!
//! Without a URL each test returns early.
#![cfg(any(feature = "persistence-postgres", feature = "persistence-redis"))]

use anyhow::Result;
use async_trait::async_trait;
use ranvier_core::{Bus, Outcome};
use ranvier_runtime::{
    Axon, CompensationContext, CompensationHandle, CompensationHook, CompletionState,
    PersistedTrace, PersistenceEnvelope, PersistenceHandle, PersistenceStore, PersistenceTraceId,
};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[derive(Clone, Default)]
struct RecordingHook {
    calls: Arc<Mutex<Vec<CompensationContext>>>,
}

#[async_trait]
impl CompensationHook for RecordingHook {
    async fn compensate(&self, context: CompensationContext) -> Result<()> {
        self.calls.lock().unwrap().push(context);
        Ok(())
    }
}

fn kinds_and_steps(trace: &PersistedTrace) -> Vec<(String, u64)> {
    trace
        .events
        .iter()
        .map(|event| (event.outcome_kind.clone(), event.step))
        .collect()
}

fn expected(rows: &[(&str, u64)]) -> Vec<(String, u64)> {
    rows.iter()
        .map(|(kind, step)| (kind.to_string(), *step))
        .collect()
}

/// G0-1: a two-step success run stores its terminal event after the last
/// transition row instead of colliding with step 1.
async fn assert_success_terminal_row(store: Arc<dyn PersistenceStore>) {
    let trace_id = format!("g0-1-success-{}", Uuid::new_v4().simple());
    let mut bus = Bus::new();
    bus.insert(PersistenceHandle::from_arc(store.clone()));
    bus.insert(PersistenceTraceId::new(trace_id.clone()));

    let axon = Axon::<i32, i32, String>::start("G01AdapterSuccess")
        .then_fn("add_one", |v: i32, _bus: &mut Bus| Outcome::next(v + 1))
        .then_fn("double", |v: i32, _bus: &mut Bus| Outcome::next(v * 2));
    let outcome = axon.execute(1, &(), &mut bus).await;
    assert!(matches!(outcome, Outcome::Next(4)), "outcome {outcome:?}");

    let trace = store.load(&trace_id).await.unwrap().unwrap();
    println!(
        "[success] {:?} completion={:?}",
        kinds_and_steps(&trace),
        trace.completion
    );
    assert_eq!(
        kinds_and_steps(&trace),
        expected(&[("Enter", 0), ("Next", 1), ("Next", 2), ("Next", 3)])
    );
    assert_eq!(
        trace.events[3].payload,
        Some(serde_json::json!({ "Next": 4 }))
    );
    assert_eq!(trace.completion, Some(CompletionState::Success));
}

/// G0-1: a compensated fault stores `Fault` and `Compensated` rows under
/// unique steps, and the hook still sees the 0.51.0 `fault_step`.
async fn assert_compensated_rows(store: Arc<dyn PersistenceStore>) {
    let trace_id = format!("g0-1-compensated-{}", Uuid::new_v4().simple());
    let hook = RecordingHook::default();
    let mut bus = Bus::new();
    bus.insert(PersistenceHandle::from_arc(store.clone()));
    bus.insert(PersistenceTraceId::new(trace_id.clone()));
    bus.insert(CompensationHandle::from_hook(hook.clone()));

    let axon = Axon::<i32, i32, String>::start("G01AdapterCompensated")
        .then_fn("add_one", |v: i32, _bus: &mut Bus| Outcome::next(v + 1))
        .then_fn("boom", |_v: i32, _bus: &mut Bus| {
            Outcome::<i32, String>::Fault("boom".to_string())
        })
        .then_fn("add_ten", |v: i32, _bus: &mut Bus| Outcome::next(v + 10));
    let outcome = axon.execute(5, &(), &mut bus).await;
    assert!(
        matches!(&outcome, Outcome::Fault(msg) if msg == "boom"),
        "outcome {outcome:?}"
    );

    let trace = store.load(&trace_id).await.unwrap().unwrap();
    println!(
        "[compensated] {:?} completion={:?}",
        kinds_and_steps(&trace),
        trace.completion
    );
    assert_eq!(
        kinds_and_steps(&trace),
        expected(&[
            ("Enter", 0),
            ("Next", 1),
            ("Fault", 2),
            ("Fault", 3),
            ("Compensated", 4),
        ])
    );
    assert_eq!(trace.completion, Some(CompletionState::Compensated));
    assert_eq!(hook.calls.lock().unwrap()[0].fault_step, 1);
}

async fn seed_step1(store: &Arc<dyn PersistenceStore>, trace_id: &str, payload: serde_json::Value) {
    for (step, kind, payload) in [(0, "Enter", None), (1, "Next", Some(payload))] {
        store
            .append(PersistenceEnvelope {
                trace_id: trace_id.to_string(),
                circuit: "G05AdapterResume".to_string(),
                schematic_version: "1.0".to_string(),
                step,
                node_id: None,
                outcome_kind: kind.to_string(),
                timestamp_ms: 0,
                payload_hash: None,
                payload,
            })
            .await
            .unwrap();
    }
}

async fn resume_nullable(
    store: Arc<dyn PersistenceStore>,
    step1_payload: serde_json::Value,
) -> Outcome<i32, String> {
    let trace_id = format!("g0-5-{}", Uuid::new_v4().simple());
    seed_step1(&store, &trace_id, step1_payload).await;

    let mut bus = Bus::new();
    bus.insert(PersistenceHandle::from_arc(store));
    bus.insert(PersistenceTraceId::new(trace_id));

    let axon = Axon::<String, String, String>::start("G05AdapterResume")
        .then_fn("parse", |s: String, _bus: &mut Bus| {
            Outcome::next(s.parse::<i32>().ok())
        })
        .then_fn("resolve", |v: Option<i32>, _bus: &mut Bus| {
            Outcome::next(v.unwrap_or(-1))
        });
    axon.execute("not-a-number".to_string(), &(), &mut bus)
        .await
}

/// G0-5: a bare `null` read back from a real store is not used as resume
/// state, while `{"Next": ...}` encodings still are.
async fn assert_bare_null_resume(store: Arc<dyn PersistenceStore>) {
    let outcome = resume_nullable(store.clone(), serde_json::Value::Null).await;
    println!("[bare null] {outcome:?}");
    assert!(
        matches!(&outcome, Outcome::Emit(name, _) if name == "execution.resumption.payload_error"),
        "outcome {outcome:?}"
    );

    let outcome = resume_nullable(store.clone(), serde_json::json!({ "Next": 7 })).await;
    assert!(matches!(outcome, Outcome::Next(7)), "outcome {outcome:?}");

    let outcome = resume_nullable(store, serde_json::json!({ "Next": null })).await;
    assert!(matches!(outcome, Outcome::Next(-1)), "outcome {outcome:?}");
}

#[cfg(feature = "persistence-postgres")]
mod postgres {
    use super::*;
    use ranvier_runtime::PostgresPersistenceStore;

    async fn store() -> Option<(Arc<dyn PersistenceStore>, sqlx::PgPool, String)> {
        let url = std::env::var("RANVIER_PERSISTENCE_POSTGRES_URL").ok()?;
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .connect(&url)
            .await
            .unwrap();
        let prefix = format!("ranvier_g0_test_{}", Uuid::new_v4().simple());
        let store = PostgresPersistenceStore::with_table_prefix(pool.clone(), prefix.clone());
        store.ensure_schema().await.unwrap();
        Some((Arc::new(store), pool, prefix))
    }

    async fn drop_tables(pool: &sqlx::PgPool, prefix: &str) {
        for suffix in ["interventions", "events", "state"] {
            sqlx::query(&format!("DROP TABLE IF EXISTS {prefix}_{suffix}"))
                .execute(pool)
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn postgres_g0_1_success_terminal_row_when_configured() {
        let Some((store, pool, prefix)) = store().await else {
            return;
        };
        assert_success_terminal_row(store).await;
        drop_tables(&pool, &prefix).await;
    }

    #[tokio::test]
    async fn postgres_g0_1_compensated_rows_when_configured() {
        let Some((store, pool, prefix)) = store().await else {
            return;
        };
        assert_compensated_rows(store).await;
        drop_tables(&pool, &prefix).await;
    }

    #[tokio::test]
    async fn postgres_g0_5_bare_null_resume_when_configured() {
        let Some((store, pool, prefix)) = store().await else {
            return;
        };
        assert_bare_null_resume(store).await;
        drop_tables(&pool, &prefix).await;
    }
}

#[cfg(feature = "persistence-redis")]
mod redis_store {
    use super::*;
    use ranvier_runtime::RedisPersistenceStore;

    /// Trace ids are unique per test; keys use the default
    /// `ranvier:persistence` prefix.
    async fn store() -> Option<Arc<dyn PersistenceStore>> {
        let url = std::env::var("RANVIER_PERSISTENCE_REDIS_URL").ok()?;
        Some(Arc::new(
            RedisPersistenceStore::connect(&url).await.unwrap(),
        ))
    }

    #[tokio::test]
    async fn redis_g0_1_success_terminal_row_when_configured() {
        let Some(store) = store().await else { return };
        assert_success_terminal_row(store).await;
    }

    #[tokio::test]
    async fn redis_g0_1_compensated_rows_when_configured() {
        let Some(store) = store().await else { return };
        assert_compensated_rows(store).await;
    }

    #[tokio::test]
    async fn redis_g0_5_bare_null_resume_when_configured() {
        let Some(store) = store().await else { return };
        assert_bare_null_resume(store).await;
    }
}
