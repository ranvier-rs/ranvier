//! P2-5 reconciliation procedures (D-FAULT-DESIGN E5/E6, case X14).
//!
//! Every procedure in the persistence operations runbook ("Reconciling an
//! unconfirmed execution") runs here against a store produced by `connect`,
//! so the same steps are checked on InMemory, PostgreSQL and Redis.

use ranvier_core::{Bus, Outcome, Transition};
use ranvier_runtime::persistence::*;
use ranvier_runtime::{Axon, ExecutionReport, PersistenceEventType, PersistenceOperation};
use std::future::Future;
use std::pin::Pin;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

pub type StoreRef = Arc<dyn PersistenceStore>;
pub type Connect = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = StoreRef> + Send>> + Send + Sync>;

/// The runbook's helper: the next event of `trace`, written by an operator.
pub fn operator_event(
    trace: &PersistedTrace,
    event_type: PersistenceEventType,
    step: u64,
    node_id: Option<String>,
    outcome_kind: &str,
    payload: Option<serde_json::Value>,
) -> PersistenceEnvelope {
    let first = trace.events.first().expect("trace has an Enter event");
    PersistenceEnvelope {
        trace_id: trace.trace_id.clone(),
        circuit: first.circuit.clone(),
        schematic_version: first.schematic_version.clone(),
        event_sequence: trace.events.len() as u64,
        event_type,
        step,
        node_id,
        outcome_kind: outcome_kind.to_string(),
        timestamp_ms: 0,
        payload_hash: None,
        payload,
    }
}

/// Counts invocations; the first `hangs` invocations never return.
#[derive(Clone)]
struct Charge {
    calls: Arc<AtomicUsize>,
    hangs: usize,
}

#[async_trait::async_trait]
impl Transition<u32, u32> for Charge {
    type Error = String;
    type Resources = ();

    async fn run(&self, input: u32, _: &(), _: &mut Bus) -> Outcome<u32, String> {
        if self.calls.fetch_add(1, Ordering::SeqCst) < self.hangs {
            std::future::pending::<()>().await;
        }
        Outcome::Next(input + 10)
    }
}

#[derive(Clone)]
struct Ship(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Transition<u32, u32> for Ship {
    type Error = String;
    type Resources = ();

    async fn run(&self, input: u32, _: &(), _: &mut Bus) -> Outcome<u32, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::Next(input + 1)
    }
}

#[derive(Clone)]
struct Reject;

#[async_trait::async_trait]
impl Transition<u32, u32> for Reject {
    type Error = String;
    type Resources = ();

    async fn run(&self, _: u32, _: &(), _: &mut Bus) -> Outcome<u32, String> {
        Outcome::Fault("rejected".into())
    }
}

#[derive(Clone)]
struct HangCompensation;

#[async_trait::async_trait]
impl CompensationHook for HangCompensation {
    async fn compensate(&self, _: CompensationContext) -> anyhow::Result<()> {
        std::future::pending().await
    }
}

/// Fails every `complete` after applying it, like a lost acknowledgement.
#[derive(Clone)]
struct LoseCompleteAck(StoreRef);

#[async_trait::async_trait]
impl PersistenceStore for LoseCompleteAck {
    async fn append(&self, envelope: PersistenceEnvelope) -> anyhow::Result<()> {
        self.0.append(envelope).await
    }
    async fn load(&self, trace_id: &str) -> anyhow::Result<Option<PersistedTrace>> {
        self.0.load(trace_id).await
    }
    async fn resume(&self, trace_id: &str, step: u64) -> anyhow::Result<ResumeCursor> {
        self.0.resume(trace_id, step).await
    }
    async fn complete(&self, _: &str, _: CompletionState) -> anyhow::Result<()> {
        anyhow::bail!("injected lost completion")
    }
    async fn save_intervention(
        &self,
        trace_id: &str,
        intervention: Intervention,
    ) -> anyhow::Result<()> {
        self.0.save_intervention(trace_id, intervention).await
    }
}

pub struct Fixture {
    label: &'static str,
    connect: Connect,
}

impl Fixture {
    pub fn new(label: &'static str, connect: Connect) -> Self {
        Self { label, connect }
    }

    fn trace_id(&self, case: &str) -> String {
        format!("rec-{case}-{}", uuid::Uuid::new_v4().simple())
    }

    fn pipeline(
        charges: &Arc<AtomicUsize>,
        hangs: usize,
        ships: &Arc<AtomicUsize>,
    ) -> Axon<u32, u32, String, ()> {
        Axon::<u32, u32, String, ()>::new("reconcile")
            .then_with_timeout(
                Charge {
                    calls: charges.clone(),
                    hangs,
                },
                Duration::from_millis(200),
                || "charge timed out".to_string(),
            )
            .then(Ship(ships.clone()))
    }

    /// Run the pipeline once with a charge that times out, leaving an open
    /// intent for the charge step.
    async fn interrupted_charge(
        &self,
        trace_id: &str,
        charges: &Arc<AtomicUsize>,
        ships: &Arc<AtomicUsize>,
    ) -> (StoreRef, PersistedTrace) {
        let store = (self.connect)().await;
        let mut bus = Bus::new();
        bus.insert(PersistenceTraceId::new(trace_id));
        bus.insert(PersistenceHandle::from_arc(store.clone()));
        let report = Self::pipeline(charges, 1, ships)
            .execute(5, &(), &mut bus)
            .await;
        assert!(
            matches!(
                &report,
                ExecutionReport::Unconfirmed {
                    operation: PersistenceOperation::Result,
                    ..
                }
            ),
            "{report:?}"
        );
        let trace = store.load(trace_id).await.unwrap().unwrap();
        (store, trace)
    }

    async fn rerun(
        &self,
        store: &StoreRef,
        trace_id: &str,
        charges: &Arc<AtomicUsize>,
        ships: &Arc<AtomicUsize>,
    ) -> ExecutionReport<Outcome<u32, String>> {
        let mut bus = Bus::new();
        bus.insert(PersistenceTraceId::new(trace_id));
        bus.insert(PersistenceHandle::from_arc(store.clone()));
        Self::pipeline(charges, 1, ships)
            .execute(5, &(), &mut bus)
            .await
    }

    fn pending(trace: &PersistedTrace) -> (u64, String) {
        match trace.reconciliation() {
            TraceReconciliation::PendingIntent {
                step,
                node_id: Some(node_id),
                ..
            } => (step, node_id),
            other => panic!("expected an open intent, got {other:?}"),
        }
    }

    /// Procedure 1: the effect ran. Record its result and run again; the
    /// execution continues after the effect.
    pub async fn effect_ran_record_result(&self) {
        let trace_id = self.trace_id("ran");
        let charges = Arc::new(AtomicUsize::new(0));
        let ships = Arc::new(AtomicUsize::new(0));
        let (store, trace) = self.interrupted_charge(&trace_id, &charges, &ships).await;
        let (step, node) = Self::pending(&trace);

        // The operator confirmed in the payment system that 5 + 10 was charged.
        let confirmed = serde_json::to_value(Outcome::<u32, String>::Next(15)).unwrap();
        store
            .append(operator_event(
                &trace,
                PersistenceEventType::Result,
                step,
                Some(node),
                "Next",
                Some(confirmed),
            ))
            .await
            .unwrap();
        assert_eq!(
            store
                .load(&trace_id)
                .await
                .unwrap()
                .unwrap()
                .reconciliation(),
            TraceReconciliation::Open
        );

        let report = self.rerun(&store, &trace_id, &charges, &ships).await;
        assert!(
            matches!(
                &report,
                ExecutionReport::Recorded {
                    result: Outcome::Next(16),
                    ..
                }
            ),
            "{report:?}"
        );
        assert_eq!(charges.load(Ordering::SeqCst), 1, "charge is not repeated");
        assert_eq!(ships.load(Ordering::SeqCst), 1);
        let trace = store.load(&trace_id).await.unwrap().unwrap();
        assert_eq!(
            trace.reconciliation(),
            TraceReconciliation::Completed(CompletionState::Success)
        );
        println!("RECONCILE store={} procedure=effect-ran PASS", self.label);
    }

    /// Procedure 2: the effect did not run and should run again. Close the
    /// intent with a Fault result and save a jump bound to the new revision.
    pub async fn effect_not_run_jump(&self) {
        let trace_id = self.trace_id("rerun");
        let charges = Arc::new(AtomicUsize::new(0));
        let ships = Arc::new(AtomicUsize::new(0));
        let (store, trace) = self.interrupted_charge(&trace_id, &charges, &ships).await;
        let (step, node) = Self::pending(&trace);

        let closed = serde_json::to_value(Outcome::<u32, String>::Fault(
            "operator: charge not executed".into(),
        ))
        .unwrap();
        store
            .append(operator_event(
                &trace,
                PersistenceEventType::Result,
                step,
                Some(node.clone()),
                "Fault",
                Some(closed),
            ))
            .await
            .unwrap();
        let trace = store.load(&trace_id).await.unwrap().unwrap();
        // Node ids are generated per Axon instance; address the node by the
        // label at the intent's step in the schematic that will run.
        let label = Self::pipeline(&charges, 1, &ships).schematic.nodes[step as usize]
            .label
            .clone();
        store
            .save_intervention(
                &trace_id,
                Intervention {
                    target_node: label,
                    payload_override: Some(serde_json::json!(5)),
                    timestamp_ms: 0,
                    expected_event_sequence: trace.events.len() as u64,
                },
            )
            .await
            .unwrap();

        let report = self.rerun(&store, &trace_id, &charges, &ships).await;
        assert!(
            matches!(
                &report,
                ExecutionReport::Recorded {
                    result: Outcome::Next(16),
                    ..
                }
            ),
            "{report:?}"
        );
        assert_eq!(charges.load(Ordering::SeqCst), 2, "charge ran once more");
        assert_eq!(ships.load(Ordering::SeqCst), 1);
        let trace = store.load(&trace_id).await.unwrap().unwrap();
        assert_eq!(
            trace.reconciliation(),
            TraceReconciliation::Completed(CompletionState::Success)
        );
        println!(
            "RECONCILE store={} procedure=effect-not-run-jump PASS",
            self.label
        );
    }

    /// Procedure 3: give the execution up. Close the intent with a Fault
    /// result, write the terminal event and complete the trace as Fault.
    pub async fn abandon(&self) {
        let trace_id = self.trace_id("abandon");
        let charges = Arc::new(AtomicUsize::new(0));
        let ships = Arc::new(AtomicUsize::new(0));
        let (store, trace) = self.interrupted_charge(&trace_id, &charges, &ships).await;
        let (step, node) = Self::pending(&trace);

        let fault =
            serde_json::to_value(Outcome::<u32, String>::Fault("operator: abandoned".into()))
                .unwrap();
        store
            .append(operator_event(
                &trace,
                PersistenceEventType::Result,
                step,
                Some(node),
                "Fault",
                Some(fault.clone()),
            ))
            .await
            .unwrap();
        let trace = store.load(&trace_id).await.unwrap().unwrap();
        store
            .append(operator_event(
                &trace,
                PersistenceEventType::Terminal,
                step + 1,
                None,
                "Fault",
                Some(fault),
            ))
            .await
            .unwrap();
        store
            .complete(&trace_id, CompletionState::Fault)
            .await
            .unwrap();

        let trace = store.load(&trace_id).await.unwrap().unwrap();
        assert_eq!(
            trace.reconciliation(),
            TraceReconciliation::Completed(CompletionState::Fault)
        );
        let report = self.rerun(&store, &trace_id, &charges, &ships).await;
        // A completed trace never runs again.
        assert!(
            matches!(
                &report,
                ExecutionReport::NotRequested {
                    result: Outcome::Emit(event, _),
                    ..
                } if event == "execution.resumption.completed_trace"
            ),
            "{report:?}"
        );
        assert_eq!(charges.load(Ordering::SeqCst), 1);
        assert_eq!(ships.load(Ordering::SeqCst), 0);
        println!("RECONCILE store={} procedure=abandon PASS", self.label);
    }

    /// Procedure 4: the terminal event is recorded but completion is not.
    /// Complete the trace with the state that matches the terminal event.
    pub async fn terminal_without_completion(&self) {
        let trace_id = self.trace_id("complete");
        let store = (self.connect)().await;
        let ships = Arc::new(AtomicUsize::new(0));
        let mut bus = Bus::new();
        bus.insert(PersistenceTraceId::new(trace_id.clone()));
        bus.insert(PersistenceHandle::from_store(LoseCompleteAck(
            store.clone(),
        )));
        let report = Axon::<u32, u32, String, ()>::new("reconcile-complete")
            .then(Ship(ships.clone()))
            .execute(1, &(), &mut bus)
            .await;
        assert!(
            matches!(
                &report,
                ExecutionReport::Unconfirmed {
                    operation: PersistenceOperation::Complete,
                    ..
                }
            ),
            "{report:?}"
        );
        let trace = store.load(&trace_id).await.unwrap().unwrap();
        assert_eq!(
            trace.reconciliation(),
            TraceReconciliation::TerminalWithoutCompletion
        );
        assert_eq!(trace.events.last().unwrap().outcome_kind, "Next");

        store
            .complete(&trace_id, CompletionState::Success)
            .await
            .unwrap();
        assert_eq!(
            store
                .load(&trace_id)
                .await
                .unwrap()
                .unwrap()
                .reconciliation(),
            TraceReconciliation::Completed(CompletionState::Success)
        );
        println!(
            "RECONCILE store={} procedure=terminal-without-completion PASS",
            self.label
        );
    }

    /// Procedure 5: compensation started and its result is unknown. After
    /// confirming the compensation in the external system, record it, write
    /// the terminal event and complete the trace as Compensated.
    pub async fn open_compensation(&self) {
        let trace_id = self.trace_id("compensation");
        let store = (self.connect)().await;
        let mut bus = Bus::new();
        bus.insert(PersistenceTraceId::new(trace_id.clone()));
        bus.insert(PersistenceHandle::from_arc(store.clone()));
        bus.insert(CompensationHandle::from_hook(HangCompensation));
        bus.insert(CleanupBudget(Duration::from_millis(200)));
        let report = Axon::<u32, u32, String, ()>::new("reconcile-compensation")
            .then(Reject)
            .execute(1, &(), &mut bus)
            .await;
        assert!(
            matches!(
                &report,
                ExecutionReport::Unconfirmed {
                    operation: PersistenceOperation::PendingIntent,
                    ..
                }
            ),
            "{report:?}"
        );
        let trace = store.load(&trace_id).await.unwrap().unwrap();
        let (step, node) = Self::pending(&trace);
        assert_eq!(node, "external-compensation");
        let fault = trace
            .events
            .iter()
            .rev()
            .find(|event| event.event_type == PersistenceEventType::Result)
            .and_then(|event| event.payload.clone());

        store
            .append(operator_event(
                &trace,
                PersistenceEventType::Compensation,
                step,
                Some(node),
                "Compensated",
                Some(serde_json::json!({"outcome": "completed"})),
            ))
            .await
            .unwrap();
        let trace = store.load(&trace_id).await.unwrap().unwrap();
        store
            .append(operator_event(
                &trace,
                PersistenceEventType::Terminal,
                step + 1,
                None,
                "Fault",
                fault,
            ))
            .await
            .unwrap();
        store
            .complete(&trace_id, CompletionState::Compensated)
            .await
            .unwrap();
        assert_eq!(
            store
                .load(&trace_id)
                .await
                .unwrap()
                .unwrap()
                .reconciliation(),
            TraceReconciliation::Completed(CompletionState::Compensated)
        );
        println!(
            "RECONCILE store={} procedure=open-compensation PASS",
            self.label
        );
    }

    /// Two operators act on the same open intent: the second write loses
    /// with `PersistenceConflict` and changes nothing.
    pub async fn concurrent_operators_conflict(&self) {
        let trace_id = self.trace_id("conflict");
        let charges = Arc::new(AtomicUsize::new(0));
        let ships = Arc::new(AtomicUsize::new(0));
        let (store, trace) = self.interrupted_charge(&trace_id, &charges, &ships).await;
        let (step, node) = Self::pending(&trace);
        let ran = operator_event(
            &trace,
            PersistenceEventType::Result,
            step,
            Some(node.clone()),
            "Next",
            Some(serde_json::to_value(Outcome::<u32, String>::Next(15)).unwrap()),
        );
        let abandoned = operator_event(
            &trace,
            PersistenceEventType::Result,
            step,
            Some(node),
            "Fault",
            Some(serde_json::to_value(Outcome::<u32, String>::Fault("x".into())).unwrap()),
        );
        store.append(ran).await.unwrap();
        let before = store.load(&trace_id).await.unwrap().unwrap();
        let error = store.append(abandoned).await.unwrap_err();
        let conflict = error
            .downcast_ref::<PersistenceConflict>()
            .unwrap_or_else(|| panic!("expected a conflict, got {error:#}"));
        assert!(
            matches!(
                conflict.kind,
                PersistenceConflictKind::StaleRevision { .. }
                    | PersistenceConflictKind::ConcurrentUpdate
            ),
            "{conflict:?}"
        );
        assert_eq!(store.load(&trace_id).await.unwrap().unwrap(), before);
        println!(
            "RECONCILE store={} procedure=operator-conflict PASS",
            self.label
        );
    }

    pub async fn run_all(&self) {
        self.effect_ran_record_result().await;
        self.effect_not_run_jump().await;
        self.abandon().await;
        self.terminal_without_completion().await;
        self.open_compensation().await;
        self.concurrent_operators_conflict().await;
    }
}
