//! P2-5 runbook reconciliation procedures (D-FAULT-DESIGN E5/E6, X14) and
//! `PersistedTrace::reconciliation` classification. InMemory runs by default.
//! PostgreSQL and Redis run with `--ignored` against disposable instances
//! named by `RANVIER_PERSISTENCE_POSTGRES_URL` and
//! `RANVIER_PERSISTENCE_REDIS_URL`.

#[path = "support/reconciliation.rs"]
mod reconciliation;

use ranvier_runtime::PersistenceEventType;
use ranvier_runtime::persistence::*;
use reconciliation::{Connect, Fixture, StoreRef, operator_event};
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn in_memory_reconciliation_procedures() {
    let store = InMemoryPersistenceStore::new();
    let connect: Connect = Arc::new(move || {
        let store = store.clone();
        Box::pin(async move { Arc::new(store) as StoreRef })
    });
    Fixture::new("in-memory", connect).run_all().await;
}

#[cfg(feature = "persistence-postgres")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated PostgreSQL fixture URL"]
async fn postgres_reconciliation_procedures() {
    let url = std::env::var("RANVIER_PERSISTENCE_POSTGRES_URL")
        .expect("explicit isolated PostgreSQL fixture URL required");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(8)
        .connect(&url)
        .await
        .unwrap();
    let prefix = format!("rrc_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
    PostgresPersistenceStore::with_table_prefix(pool.clone(), prefix.clone())
        .ensure_schema()
        .await
        .unwrap();
    let connect: Connect = Arc::new(move || {
        let store = PostgresPersistenceStore::with_table_prefix(pool.clone(), prefix.clone());
        Box::pin(async move { Arc::new(store) as StoreRef })
    });
    Fixture::new("postgres", connect).run_all().await;
}

#[cfg(feature = "persistence-redis")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated Redis fixture URL"]
async fn redis_reconciliation_procedures() {
    let url = std::env::var("RANVIER_PERSISTENCE_REDIS_URL")
        .expect("explicit isolated Redis fixture URL required");
    let connect: Connect = Arc::new(move || {
        let url = url.clone();
        Box::pin(async move {
            Arc::new(RedisPersistenceStore::connect(&url).await.unwrap()) as StoreRef
        })
    });
    Fixture::new("redis", connect).run_all().await;
}

fn trace(events: Vec<PersistenceEnvelope>, completion: Option<CompletionState>) -> PersistedTrace {
    PersistedTrace {
        trace_id: "classify".into(),
        circuit: "classify".into(),
        schematic_version: "1".into(),
        format_version: PERSISTENCE_FORMAT_VERSION,
        events,
        interventions: Vec::new(),
        resumed_from_step: None,
        completion,
    }
}

fn push(
    trace: &mut PersistedTrace,
    event_type: PersistenceEventType,
    step: u64,
    node: Option<&str>,
    kind: &str,
) {
    let mut envelope = operator_event(
        trace,
        event_type,
        step,
        node.map(str::to_string),
        kind,
        Some(serde_json::json!({ "Next": 1 })),
    );
    if trace.events.is_empty() {
        envelope.event_sequence = 0;
    }
    trace.events.push(envelope);
}

fn enter() -> PersistedTrace {
    let mut trace = trace(Vec::new(), None);
    trace.events.push(PersistenceEnvelope {
        trace_id: "classify".into(),
        circuit: "classify".into(),
        schematic_version: "1".into(),
        event_sequence: 0,
        event_type: PersistenceEventType::Enter,
        step: 0,
        node_id: Some("start".into()),
        outcome_kind: "Enter".into(),
        timestamp_ms: 0,
        payload_hash: None,
        payload: Some(serde_json::json!({ "input": 1 })),
    });
    trace
}

#[test]
fn reconciliation_classifies_every_state() {
    use PersistenceEventType as T;
    use TraceReconciliation as R;

    let open = enter();
    assert_eq!(open.reconciliation(), R::Open);

    let mut pending = enter();
    push(&mut pending, T::Intent, 1, Some("charge"), "Intent");
    assert_eq!(
        pending.reconciliation(),
        R::PendingIntent {
            event_sequence: 1,
            step: 1,
            node_id: Some("charge".into()),
        }
    );

    let mut settled = pending.clone();
    push(&mut settled, T::Result, 1, Some("charge"), "Next");
    assert_eq!(settled.reconciliation(), R::Open);

    let mut terminal = settled.clone();
    push(&mut terminal, T::Terminal, 2, None, "Next");
    assert_eq!(terminal.reconciliation(), R::TerminalWithoutCompletion);

    let mut completed = terminal.clone();
    completed.completion = Some(CompletionState::Success);
    assert_eq!(
        completed.reconciliation(),
        R::Completed(CompletionState::Success)
    );

    let mut no_terminal = settled.clone();
    no_terminal.completion = Some(CompletionState::Success);
    assert_eq!(no_terminal.reconciliation(), R::CompletedWithoutTerminal);

    let mut gap = settled.clone();
    gap.events[2].event_sequence = 7;
    assert!(matches!(gap.reconciliation(), R::Invalid { .. }));

    let mut old_format = settled;
    old_format.format_version = PERSISTENCE_FORMAT_VERSION + 1;
    assert!(matches!(old_format.reconciliation(), R::Invalid { .. }));
}
