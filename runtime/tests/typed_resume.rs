//! P2-3 typed resume (D-RESUME, design R1-R7): stored checkpoints are the
//! authority, and a trace is continued only with the input that started it.

use ranvier_core::cancellation::{CancellationReason, CancellationToken};
use ranvier_core::schematic::{MigrationRegistry, MigrationStrategy, SnapshotMigration};
use ranvier_core::{Bus, Outcome, Transition};
use ranvier_runtime::persistence::*;
use ranvier_runtime::{Axon, ExecutionReport, ExecutionTerminal, PersistenceOperation};
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

fn report_result<T: Clone>(report: &ExecutionReport<T>) -> Option<T> {
    match report {
        ExecutionReport::Unpersisted { result, .. }
        | ExecutionReport::NotRequested { result, .. }
        | ExecutionReport::Recorded { result, .. }
        | ExecutionReport::Unconfirmed { result, .. } => Some(result.clone()),
        ExecutionReport::BlockedBeforeEffect { .. } => None,
    }
}

/// The `Next` value of a report, if it has one.
fn next_value<T: Clone, E: Clone>(report: &ExecutionReport<Outcome<T, E>>) -> Option<T> {
    match report_result(report)? {
        Outcome::Next(value) => Some(value),
        _ => None,
    }
}

fn blocked_with(report_operation: Option<PersistenceOperation>, expected: PersistenceOperation) {
    assert_eq!(report_operation, Some(expected));
}

fn blocked_operation<T>(report: &ExecutionReport<T>) -> Option<PersistenceOperation> {
    match report {
        ExecutionReport::BlockedBeforeEffect { operation, .. } => Some(*operation),
        _ => None,
    }
}

fn bus_for(store: &InMemoryPersistenceStore, trace_id: &str) -> Bus {
    let mut bus = Bus::new();
    bus.insert(PersistenceTraceId::new(trace_id));
    bus.insert(PersistenceHandle::from_store(store.clone()));
    bus
}

type Seed = (
    PersistenceEventType,
    u64,
    &'static str,
    Option<usize>,
    Option<serde_json::Value>,
);

/// Append a journal that stopped without a terminal (a crashed invocation).
async fn seed_events(
    store: &InMemoryPersistenceStore,
    trace_id: &str,
    schematic: &ranvier_core::schematic::Schematic,
    version: &str,
    events: Vec<Seed>,
) {
    for (sequence, (event_type, step, kind, node, payload)) in events.into_iter().enumerate() {
        store
            .append(PersistenceEnvelope {
                trace_id: trace_id.into(),
                circuit: schematic.name.clone(),
                schematic_version: version.into(),
                event_sequence: sequence as u64,
                event_type,
                step,
                node_id: node.map(|index| schematic.nodes[index].id.clone()),
                outcome_kind: kind.into(),
                timestamp_ms: 0,
                payload_hash: None,
                payload,
            })
            .await
            .unwrap();
    }
}

fn effect_step(
    step: u64,
    node: usize,
    input: serde_json::Value,
    output: serde_json::Value,
) -> [Seed; 2] {
    [
        (
            PersistenceEventType::Intent,
            step,
            "Intent",
            Some(node),
            Some(input),
        ),
        (
            PersistenceEventType::Result,
            step,
            "Next",
            Some(node),
            Some(serde_json::json!({ "Next": output })),
        ),
    ]
}

fn entry(input: serde_json::Value) -> Seed {
    (
        PersistenceEventType::Enter,
        0,
        "Enter",
        None,
        Some(serde_json::json!({ "input": input })),
    )
}

async fn seed_enter(
    store: &InMemoryPersistenceStore,
    trace_id: &str,
    axon_name: &str,
    version: &str,
    payload: Option<serde_json::Value>,
) {
    store
        .append(PersistenceEnvelope {
            trace_id: trace_id.into(),
            circuit: axon_name.into(),
            schematic_version: version.into(),
            event_sequence: 0,
            event_type: PersistenceEventType::Enter,
            step: 0,
            node_id: None,
            outcome_kind: "Enter".into(),
            timestamp_ms: 0,
            payload_hash: None,
            payload,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn new_trace_records_its_input_on_the_entry_marker() {
    let store = InMemoryPersistenceStore::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let axon = Axon::<u32, u32, String>::new("record-input").then(AddOne(calls.clone()));
    let mut bus = bus_for(&store, "record-input");
    let report = axon.execute(41, &(), &mut bus).await;
    assert_eq!(next_value(&report), Some(42));
    let trace = store.load("record-input").await.unwrap().unwrap();
    assert_eq!(trace.events[0].event_type, PersistenceEventType::Enter);
    assert_eq!(
        trace.events[0].payload,
        Some(serde_json::json!({"input": 41}))
    );
    // Later markers carry no payload.
    assert!(
        trace.events[1..]
            .iter()
            .filter(|event| event.event_type == PersistenceEventType::Enter)
            .all(|event| event.payload.is_none())
    );
}

#[tokio::test]
async fn crashed_trace_continues_from_the_stored_checkpoint_with_the_same_input() {
    let store = InMemoryPersistenceStore::new();
    let calls: Vec<_> = (0..3).map(|_| Arc::new(AtomicUsize::new(0))).collect();
    let axon = Axon::<u32, u32, String>::new("crash-resume")
        .then(AddOne(calls[0].clone()))
        .then(AddOne(calls[1].clone()))
        .then(AddOne(calls[2].clone()));
    let schematic = axon.schematic().clone();
    let version = schematic.schema_version.clone();
    // Input 1; step 1 stored 5 (not 2) so the checkpoint is observable.
    let mut events = vec![entry(serde_json::json!(1))];
    events.extend(effect_step(
        1,
        1,
        serde_json::json!(1),
        serde_json::json!(5),
    ));
    seed_events(&store, "crash-resume", &schematic, &version, events).await;
    let before = store.load("crash-resume").await.unwrap().unwrap();
    let mut bus = bus_for(&store, "crash-resume");

    // A different caller input is refused before any marker or effect.
    let report = axon.execute(9, &(), &mut bus).await;
    blocked_with(
        blocked_operation(&report),
        PersistenceOperation::ResumeInput,
    );
    assert_eq!(store.load("crash-resume").await.unwrap().unwrap(), before);

    // The same input continues at step 2 with the stored value 5.
    let report = axon.execute(1, &(), &mut bus).await;
    assert_eq!(next_value(&report), Some(7));
    let counts: Vec<_> = calls.iter().map(|c| c.load(Ordering::SeqCst)).collect();
    assert_eq!(counts, vec![0, 1, 1]);
}

#[tokio::test]
async fn operator_resume_anchor_feeds_the_next_step() {
    let store = InMemoryPersistenceStore::new();
    let calls: Vec<_> = (0..3).map(|_| Arc::new(AtomicUsize::new(0))).collect();
    let axon = Axon::<u32, u32, String>::new("anchor-resume")
        .then(AddOne(calls[0].clone()))
        .then(AddOne(calls[1].clone()))
        .then(AddOne(calls[2].clone()));
    let schematic = axon.schematic().clone();
    let version = schematic.schema_version.clone();
    let mut events = vec![entry(serde_json::json!(1))];
    events.extend(effect_step(
        1,
        1,
        serde_json::json!(1),
        serde_json::json!(5),
    ));
    events.extend(effect_step(
        2,
        2,
        serde_json::json!(5),
        serde_json::json!(50),
    ));
    seed_events(&store, "anchor-resume", &schematic, &version, events).await;
    // Rewind to after step 1: step 2 reruns from the step-1 checkpoint.
    store.resume("anchor-resume", 1).await.unwrap();
    let mut bus = bus_for(&store, "anchor-resume");
    let report = axon.execute(1, &(), &mut bus).await;
    assert_eq!(next_value(&report), Some(7));
    let counts: Vec<_> = calls.iter().map(|c| c.load(Ordering::SeqCst)).collect();
    assert_eq!(counts, vec![0, 1, 1]);
}

#[tokio::test]
async fn entry_only_trace_restarts_from_the_beginning() {
    let store = InMemoryPersistenceStore::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let axon = Axon::<u32, u32, String>::new("entry-only")
        .then(AddOne(calls.clone()))
        .then(AddOne(calls.clone()));
    let version = axon.schematic().schema_version.clone();
    seed_enter(
        &store,
        "entry-only",
        "entry-only",
        &version,
        Some(serde_json::json!({"input": 5})),
    )
    .await;

    let mut bus = bus_for(&store, "entry-only");
    let report = axon.execute(6, &(), &mut bus).await;
    blocked_with(
        blocked_operation(&report),
        PersistenceOperation::ResumeInput,
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let report = axon.execute(5, &(), &mut bus).await;
    assert_eq!(next_value(&report), Some(7));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let trace = store.load("entry-only").await.unwrap().unwrap();
    let entries: Vec<_> = trace
        .events
        .iter()
        .filter(|event| event.event_type == PersistenceEventType::Enter)
        .map(|event| (event.step, event.payload.clone()))
        .collect();
    assert_eq!(
        entries,
        vec![
            (0, Some(serde_json::json!({"input": 5}))),
            (0, Some(serde_json::json!({"input": 5}))),
        ]
    );
}

#[tokio::test]
async fn trace_without_a_recorded_input_is_not_continued() {
    let store = InMemoryPersistenceStore::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let axon = Axon::<u32, u32, String>::new("no-input").then(AddOne(calls.clone()));
    let version = axon.schematic().schema_version.clone();
    seed_enter(&store, "no-input", "no-input", &version, None).await;
    let before = store.load("no-input").await.unwrap().unwrap();

    let mut bus = bus_for(&store, "no-input");
    let report = axon.execute(5, &(), &mut bus).await;
    blocked_with(
        blocked_operation(&report),
        PersistenceOperation::ResumeInput,
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(store.load("no-input").await.unwrap().unwrap(), before);
}

#[derive(Clone)]
struct MaybeNext(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Transition<(), Option<u32>> for MaybeNext {
    type Error = String;
    type Resources = ();

    async fn run(&self, _: (), _: &(), _: &mut Bus) -> Outcome<Option<u32>, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::Next(None)
    }
}

#[derive(Clone)]
struct Describe(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Transition<Option<u32>, String> for Describe {
    type Error = String;
    type Resources = ();

    async fn run(&self, input: Option<u32>, _: &(), _: &mut Bus) -> Outcome<String, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::Next(format!("{input:?}"))
    }
}

#[tokio::test]
async fn unit_input_and_nullable_checkpoint_resume() {
    let store = InMemoryPersistenceStore::new();
    let first = Arc::new(AtomicUsize::new(0));
    let second = Arc::new(AtomicUsize::new(0));
    let axon = Axon::<(), (), String>::new("nullable-resume")
        .then(MaybeNext(first.clone()))
        .then(Describe(second.clone()));
    let schematic = axon.schematic().clone();
    let events = [
        (
            PersistenceEventType::Enter,
            0,
            "Enter",
            None,
            Some(serde_json::json!({"input": null})),
        ),
        (
            PersistenceEventType::Intent,
            1,
            "Intent",
            Some(1),
            Some(serde_json::json!(null)),
        ),
        (
            PersistenceEventType::Result,
            1,
            "Next",
            Some(1),
            Some(serde_json::json!({"Next": null})),
        ),
    ];
    for (sequence, (event_type, step, kind, node, payload)) in events.into_iter().enumerate() {
        store
            .append(PersistenceEnvelope {
                trace_id: "nullable-resume".into(),
                circuit: schematic.name.clone(),
                schematic_version: schematic.schema_version.clone(),
                event_sequence: sequence as u64,
                event_type,
                step,
                node_id: node.map(|index: usize| schematic.nodes[index].id.clone()),
                outcome_kind: kind.into(),
                timestamp_ms: 0,
                payload_hash: None,
                payload,
            })
            .await
            .unwrap();
    }
    let mut bus = bus_for(&store, "nullable-resume");
    let report = axon.execute((), &(), &mut bus).await;
    assert_eq!(next_value(&report), Some("None".to_string()));
    assert_eq!(first.load(Ordering::SeqCst), 0);
    assert_eq!(second.load(Ordering::SeqCst), 1);
}

#[derive(Clone)]
struct CountHook(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl CompensationHook for CountHook {
    async fn compensate(&self, _: CompensationContext) -> anyhow::Result<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn pre_cancelled_continuation_checks_the_input_before_cleanup() {
    for same_input in [false, true] {
        let store = InMemoryPersistenceStore::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let hooks = Arc::new(AtomicUsize::new(0));
        let axon = Axon::<u32, u32, String>::new("cancelled-resume").then(AddOne(calls.clone()));
        let version = axon.schematic().schema_version.clone();
        seed_enter(
            &store,
            "cancelled-resume",
            "cancelled-resume",
            &version,
            Some(serde_json::json!({"input": 3})),
        )
        .await;
        let before = store.load("cancelled-resume").await.unwrap().unwrap();
        let mut bus = bus_for(&store, "cancelled-resume");
        bus.insert(CompensationHandle::from_hook(CountHook(hooks.clone())));
        let token = CancellationToken::new();
        token.cancel(CancellationReason::Explicit);
        let input = if same_input { 3 } else { 4 };
        let report = axon.execute_cancellable(input, &(), &mut bus, token).await;
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        if same_input {
            assert!(
                matches!(
                    report_result(&report),
                    Some(ExecutionTerminal::Cancelled(_))
                ),
                "{report:?}"
            );
            assert_eq!(hooks.load(Ordering::SeqCst), 1);
        } else {
            blocked_with(
                blocked_operation(&report),
                PersistenceOperation::ResumeInput,
            );
            assert_eq!(
                hooks.load(Ordering::SeqCst),
                0,
                "no cleanup for a foreign input"
            );
            assert_eq!(
                store.load("cancelled-resume").await.unwrap().unwrap(),
                before
            );
        }
    }
}

#[tokio::test]
async fn manual_jump_requires_the_trace_input_and_uses_its_override() {
    for same_input in [false, true] {
        let store = InMemoryPersistenceStore::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let axon = Axon::<u32, u32, String>::new("jump-resume")
            .then(AddOne(calls.clone()))
            .then(AddOne(calls.clone()));
        let version = axon.schematic().schema_version.clone();
        seed_enter(
            &store,
            "jump-resume",
            "jump-resume",
            &version,
            Some(serde_json::json!({"input": 1})),
        )
        .await;
        store
            .save_intervention(
                "jump-resume",
                Intervention {
                    target_node: axon.schematic().nodes[2].id.clone(),
                    payload_override: Some(serde_json::json!(100)),
                    timestamp_ms: 0,
                },
            )
            .await
            .unwrap();
        let mut bus = bus_for(&store, "jump-resume");
        let report = axon
            .execute(if same_input { 1 } else { 2 }, &(), &mut bus)
            .await;
        if same_input {
            assert_eq!(next_value(&report), Some(101));
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        } else {
            blocked_with(
                blocked_operation(&report),
                PersistenceOperation::ResumeInput,
            );
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        }
    }
}

#[tokio::test]
async fn resume_from_start_migration_uses_the_caller_input() {
    let store = InMemoryPersistenceStore::new();
    let old = Axon::<u32, u32, String>::new("migrated")
        .with_version("v1")
        .then(AddOne(Arc::new(AtomicUsize::new(0))))
        .then(AddOne(Arc::new(AtomicUsize::new(0))));
    let old_schematic = old.schematic().clone();
    let mut events = vec![entry(serde_json::json!(1))];
    events.extend(effect_step(
        1,
        1,
        serde_json::json!(1),
        serde_json::json!(2),
    ));
    seed_events(&store, "migrated", &old_schematic, "v1", events).await;

    let new_calls = Arc::new(AtomicUsize::new(0));
    let new = Axon::<u32, u32, String>::new("migrated")
        .with_version("v2")
        .then(AddOne(new_calls.clone()))
        .then(AddOne(new_calls.clone()));
    let mut registry = MigrationRegistry::new("migrated");
    registry.register(SnapshotMigration {
        name: Some("restart".into()),
        from_version: "v1".into(),
        to_version: "v2".into(),
        default_strategy: MigrationStrategy::ResumeFromStart,
        node_mapping: Default::default(),
        payload_mapper: None,
    });
    let mut bus = bus_for(&store, "migrated");
    bus.insert(registry);
    // The owner-registered migration restarts with the caller's new input.
    let report = new.execute(10, &(), &mut bus).await;
    assert_eq!(next_value(&report), Some(12));
    assert_eq!(new_calls.load(Ordering::SeqCst), 2);
    let trace = store.load("migrated").await.unwrap().unwrap();
    let latest_entry = trace
        .events
        .iter()
        .rev()
        .find(|event| event.event_type == PersistenceEventType::Enter && event.step == 0)
        .unwrap();
    assert_eq!(latest_entry.payload, Some(serde_json::json!({"input": 10})));
}
