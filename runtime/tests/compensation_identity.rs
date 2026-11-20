//! COMPENSATION-IDENTITY-01: the compensation marker key and the hook context
//! name one compensation effect through the journal's intent `event_sequence`.

use ranvier_core::{Bus, Outcome, Transition};
use ranvier_runtime::persistence::*;
use ranvier_runtime::{Axon, ExecutionReport};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

const CIRCUIT: &str = "CompensationIdentity";

#[derive(Clone)]
struct BusinessFault;

#[async_trait::async_trait]
impl Transition<u32, u32> for BusinessFault {
    type Error = String;
    type Resources = ();

    async fn run(&self, _: u32, _: &(), _: &mut Bus) -> Outcome<u32, String> {
        Outcome::Fault("business-fault".into())
    }
}

#[derive(Clone)]
struct PassThrough;

#[async_trait::async_trait]
impl Transition<u32, u32> for PassThrough {
    type Error = String;
    type Resources = ();

    async fn run(&self, value: u32, _: &(), _: &mut Bus) -> Outcome<u32, String> {
        Outcome::Next(value)
    }
}

#[derive(Clone, Default)]
struct RecordingHook {
    calls: Arc<AtomicUsize>,
    contexts: Arc<Mutex<Vec<CompensationContext>>>,
}

#[async_trait::async_trait]
impl CompensationHook for RecordingHook {
    async fn compensate(&self, context: CompensationContext) -> anyhow::Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.contexts.lock().unwrap().push(context);
        Ok(())
    }
}

/// Documented v2 marker key with a trace store.
fn journaled_key(trace_id: &str, circuit: &str, intent_sequence: u64) -> String {
    format!(
        "v2:j:{intent_sequence}:{}:{trace_id}:{}:{circuit}",
        trace_id.len(),
        circuit.len()
    )
}

async fn compensation_intent_sequence(store: &InMemoryPersistenceStore, trace_id: &str) -> u64 {
    let trace = store
        .load(trace_id)
        .await
        .unwrap()
        .expect("trace persisted");
    let intents: Vec<_> = trace
        .events
        .iter()
        .filter(|event| {
            event.node_id.as_deref() == Some("external-compensation")
                && event.outcome_kind == "Intent"
        })
        .collect();
    assert_eq!(intents.len(), 1, "one compensation intent per trace");
    intents[0].event_sequence
}

struct Run {
    hook: RecordingHook,
    trace_store: InMemoryPersistenceStore,
}

async fn run_fault(
    trace_id: &str,
    markers: &InMemoryCompensationIdempotencyStore,
    with_trace_store: bool,
) -> Run {
    let hook = RecordingHook::default();
    let trace_store = InMemoryPersistenceStore::new();
    let mut bus = Bus::new();
    bus.insert(PersistenceTraceId::new(trace_id));
    if with_trace_store {
        bus.insert(PersistenceHandle::from_store(trace_store.clone()));
    }
    bus.insert(CompensationHandle::from_hook(hook.clone()));
    bus.insert(CompensationIdempotencyHandle::from_store(markers.clone()));
    let axon = Axon::<u32, u32, String, ()>::new(CIRCUIT)
        .then(PassThrough)
        .then(BusinessFault);
    let report = axon.execute(7, &(), &mut bus).await;
    let result = match &report {
        ExecutionReport::Unpersisted { result, .. }
        | ExecutionReport::NotRequested { result, .. }
        | ExecutionReport::Recorded { result, .. }
        | ExecutionReport::Unconfirmed { result, .. } => Some(result),
        ExecutionReport::BlockedBeforeEffect { .. } => None,
    };
    assert!(
        matches!(result, Some(Outcome::Fault(message)) if message == "business-fault"),
        "unexpected report: {report:?}"
    );
    Run { hook, trace_store }
}

#[tokio::test]
async fn hook_receives_the_journal_intent_sequence_and_marker_uses_it() {
    let markers = InMemoryCompensationIdempotencyStore::new();
    let trace_id = "identity-journaled";
    let run = run_fault(trace_id, &markers, true).await;

    let sequence = compensation_intent_sequence(&run.trace_store, trace_id).await;
    let contexts = run.hook.contexts.lock().unwrap().clone();
    assert_eq!(contexts.len(), 1);
    assert_eq!(contexts[0].intent_sequence, Some(sequence));
    assert_eq!(contexts[0].fault_kind, "Fault");
    assert!(
        markers
            .was_compensated(&journaled_key(trace_id, CIRCUIT, sequence))
            .await
            .unwrap()
    );
    // Version 1 keys are not written.
    assert!(
        !markers
            .was_compensated(&format!("{trace_id}:{CIRCUIT}:Fault"))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn hook_without_trace_store_has_no_intent_sequence() {
    let markers = InMemoryCompensationIdempotencyStore::new();
    let trace_id = "identity-unjournaled";
    let run = run_fault(trace_id, &markers, false).await;

    let contexts = run.hook.contexts.lock().unwrap().clone();
    assert_eq!(contexts.len(), 1);
    assert_eq!(contexts[0].intent_sequence, None);
    let key = format!(
        "v2:u:{}:5:Fault:{}:{trace_id}:{}:{CIRCUIT}",
        contexts[0].fault_step,
        trace_id.len(),
        CIRCUIT.len()
    );
    assert!(markers.was_compensated(&key).await.unwrap());
}

#[tokio::test]
async fn markers_of_other_compensations_do_not_skip_the_hook() {
    // The intent sequence of this graph's compensation is deterministic; learn
    // it from an unrelated trace first.
    let probe_markers = InMemoryCompensationIdempotencyStore::new();
    let probe = run_fault("identity-probe", &probe_markers, true).await;
    let sequence = compensation_intent_sequence(&probe.trace_store, "identity-probe").await;
    assert!(sequence > 0);

    // Same trace, circuit and fault kind; only the intent sequence differs.
    let markers = InMemoryCompensationIdempotencyStore::new();
    let trace_id = "identity-neighbours";
    for other in [sequence - 1, sequence + 1] {
        markers
            .mark_compensated(&journaled_key(trace_id, CIRCUIT, other))
            .await
            .unwrap();
    }
    // A version 1 marker for the same trace is ignored.
    markers
        .mark_compensated(&format!("{trace_id}:{CIRCUIT}:Fault"))
        .await
        .unwrap();
    let run = run_fault(trace_id, &markers, true).await;
    assert_eq!(run.hook.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        compensation_intent_sequence(&run.trace_store, trace_id).await,
        sequence
    );

    // The marker of this very compensation skips the hook.
    let markers = InMemoryCompensationIdempotencyStore::new();
    let trace_id = "identity-same";
    markers
        .mark_compensated(&journaled_key(trace_id, CIRCUIT, sequence))
        .await
        .unwrap();
    let run = run_fault(trace_id, &markers, true).await;
    assert_eq!(run.hook.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cancellation_compensation_receives_its_intent_sequence() {
    let markers = InMemoryCompensationIdempotencyStore::new();
    let hook = RecordingHook::default();
    let trace_store = InMemoryPersistenceStore::new();
    let trace_id = "identity-cancelled";
    let mut bus = Bus::new();
    bus.insert(PersistenceTraceId::new(trace_id));
    bus.insert(PersistenceHandle::from_store(trace_store.clone()));
    bus.insert(CompensationHandle::from_hook(hook.clone()));
    bus.insert(CompensationIdempotencyHandle::from_store(markers.clone()));
    let axon = Axon::<u32, u32, String, ()>::new(CIRCUIT).then(PassThrough);
    let token = ranvier_core::cancellation::CancellationToken::new();
    token.cancel(ranvier_core::cancellation::CancellationReason::Explicit);
    let report = axon.execute_cancellable(7, &(), &mut bus, token).await;
    assert!(
        !matches!(report, ExecutionReport::Unconfirmed { .. }),
        "unexpected report: {report:?}"
    );

    let contexts = hook.contexts.lock().unwrap().clone();
    assert_eq!(contexts.len(), 1, "cancellation compensation ran once");
    assert_eq!(contexts[0].fault_kind, "Cancelled");
    let sequence = compensation_intent_sequence(&trace_store, trace_id).await;
    assert_eq!(contexts[0].intent_sequence, Some(sequence));
    assert!(
        markers
            .was_compensated(&journaled_key(trace_id, CIRCUIT, sequence))
            .await
            .unwrap()
    );
}
