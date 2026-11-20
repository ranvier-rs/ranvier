//! P2-3 typed resume against real PostgreSQL and Redis stores (TYPE-04 subset).
//! Run with `--ignored` and `RANVIER_PERSISTENCE_POSTGRES_URL` /
//! `RANVIER_PERSISTENCE_REDIS_URL` pointing at disposable instances.
#![cfg(any(feature = "persistence-postgres", feature = "persistence-redis"))]

use ranvier_core::{Bus, Outcome, Transition};
use ranvier_runtime::persistence::*;
use ranvier_runtime::{Axon, ExecutionReport, PersistenceOperation};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Clone)]
struct AddOne(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Transition<u32, u32> for AddOne {
    type Error = String;
    type Resources = ();

    async fn run(&self, input: u32, _: &(), _: &mut Bus) -> Outcome<u32, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::Next(input + 1)
    }
}

struct Graph {
    axon: Axon<u32, u32, String>,
    calls: Vec<Arc<AtomicUsize>>,
}

fn graph() -> Graph {
    let calls: Vec<_> = (0..3).map(|_| Arc::new(AtomicUsize::new(0))).collect();
    let axon = Axon::<u32, u32, String>::new("typed-resume-live")
        .then(AddOne(calls[0].clone()))
        .then(AddOne(calls[1].clone()))
        .then(AddOne(calls[2].clone()));
    Graph { axon, calls }
}

impl Graph {
    fn counts(&self) -> Vec<usize> {
        self.calls
            .iter()
            .map(|c| c.load(Ordering::SeqCst))
            .collect()
    }
}

fn next_value(report: &ExecutionReport<Outcome<u32, String>>) -> Option<u32> {
    match report {
        ExecutionReport::Recorded {
            result: Outcome::Next(value),
            ..
        } => Some(*value),
        _ => None,
    }
}

fn blocked_operation<T>(report: &ExecutionReport<T>) -> Option<PersistenceOperation> {
    match report {
        ExecutionReport::BlockedBeforeEffect { operation, .. } => Some(*operation),
        _ => None,
    }
}

/// Append a journal that stopped after step 1 (a crashed invocation).
async fn seed(
    store: &Arc<dyn PersistenceStore>,
    trace_id: &str,
    graph: &Graph,
    input: u32,
    step_one: Option<serde_json::Value>,
) {
    let schematic = graph.axon.schematic();
    let mut events = vec![(
        PersistenceEventType::Enter,
        0,
        "Enter",
        None,
        Some(serde_json::json!({ "input": input })),
    )];
    if let Some(output) = step_one {
        let node = Some(schematic.nodes[1].id.clone());
        events.push((
            PersistenceEventType::Intent,
            1,
            "Intent",
            node.clone(),
            Some(serde_json::json!(input)),
        ));
        events.push((
            PersistenceEventType::Result,
            1,
            "Next",
            node,
            Some(serde_json::json!({ "Next": output })),
        ));
    }
    for (sequence, (event_type, step, kind, node_id, payload)) in events.into_iter().enumerate() {
        store
            .append(PersistenceEnvelope {
                trace_id: trace_id.into(),
                circuit: schematic.name.clone(),
                schematic_version: schematic.schema_version.clone(),
                event_sequence: sequence as u64,
                event_type,
                step,
                node_id,
                outcome_kind: kind.into(),
                timestamp_ms: 0,
                payload_hash: None,
                payload,
            })
            .await
            .unwrap();
    }
}

async fn run(
    store: &Arc<dyn PersistenceStore>,
    trace_id: &str,
    graph: &Graph,
    input: u32,
) -> ExecutionReport<Outcome<u32, String>> {
    let mut bus = Bus::new();
    bus.insert(PersistenceTraceId::new(trace_id));
    bus.insert(PersistenceHandle::from_arc(store.clone()));
    graph.axon.execute(input, &(), &mut bus).await
}

async fn typed_resume_matrix(store: Arc<dyn PersistenceStore>, label: &str) {
    let run_id = uuid::Uuid::new_v4().simple().to_string();
    let trace = |case: &str| format!("typed-resume-{case}-{run_id}");

    // New trace: the entry marker round-trips the recorded input.
    let fresh = graph();
    let id = trace("fresh");
    assert_eq!(next_value(&run(&store, &id, &fresh, 41).await), Some(44));
    let loaded = store.load(&id).await.unwrap().unwrap();
    assert_eq!(loaded.events[0].event_type, PersistenceEventType::Enter);
    assert_eq!(
        loaded.events[0].payload,
        Some(serde_json::json!({"input": 41}))
    );

    // Crashed after step 1 (stored 5): a different input is refused untouched,
    // the same input continues from the stored value.
    let crashed = graph();
    let id = trace("crashed");
    seed(&store, &id, &crashed, 1, Some(serde_json::json!(5))).await;
    let before = store.load(&id).await.unwrap().unwrap();
    assert_eq!(
        blocked_operation(&run(&store, &id, &crashed, 9).await),
        Some(PersistenceOperation::ResumeInput)
    );
    assert_eq!(store.load(&id).await.unwrap().unwrap(), before);
    assert_eq!(next_value(&run(&store, &id, &crashed, 1).await), Some(7));
    assert_eq!(crashed.counts(), vec![0, 1, 1]);

    // A checkpoint that does not decode as the step input blocks the effect.
    let incompatible = graph();
    let id = trace("incompatible");
    seed(
        &store,
        &id,
        &incompatible,
        1,
        Some(serde_json::json!("not-a-u32")),
    )
    .await;
    assert_eq!(
        blocked_operation(&run(&store, &id, &incompatible, 1).await),
        Some(PersistenceOperation::RecoveryPayload)
    );
    assert_eq!(incompatible.counts(), vec![0, 0, 0]);
    let loaded = store.load(&id).await.unwrap().unwrap();
    assert_eq!(loaded.completion, None);
    assert_eq!(
        loaded.events[2].payload,
        Some(serde_json::json!({"Next": "not-a-u32"}))
    );

    // Only an entry marker: restart from the beginning with the same input.
    let entry_only = graph();
    let id = trace("entry-only");
    seed(&store, &id, &entry_only, 5, None).await;
    assert_eq!(next_value(&run(&store, &id, &entry_only, 5).await), Some(8));
    assert_eq!(entry_only.counts(), vec![1, 1, 1]);

    println!("TYPED_RESUME_LIVE store={label} cases=fresh,crashed,incompatible,entry-only PASS");
}

#[cfg(feature = "persistence-postgres")]
#[tokio::test]
#[ignore = "requires an isolated PostgreSQL fixture URL"]
async fn postgres_typed_resume_matrix() {
    let url = std::env::var("RANVIER_PERSISTENCE_POSTGRES_URL")
        .expect("explicit isolated PostgreSQL fixture URL required");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(3)
        .connect(&url)
        .await
        .unwrap();
    let prefix = format!("rtr_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
    let store = PostgresPersistenceStore::with_table_prefix(pool, prefix);
    store.ensure_schema().await.unwrap();
    typed_resume_matrix(Arc::new(store), "postgres").await;
}

#[cfg(feature = "persistence-redis")]
#[tokio::test]
#[ignore = "requires an isolated Redis fixture URL"]
async fn redis_typed_resume_matrix() {
    let url = std::env::var("RANVIER_PERSISTENCE_REDIS_URL")
        .expect("explicit isolated Redis fixture URL required");
    let store = RedisPersistenceStore::connect(&url).await.unwrap();
    typed_resume_matrix(Arc::new(store), "redis").await;
}
