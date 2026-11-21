//! P2-5 fault injection (D-FAULT-DESIGN E1-E8, cases X1-X14).
//!
//! Faults are injected with fake stores, fake hooks, barriers and paused
//! Tokio time; no case depends on wall-clock sleeps.

use ranvier_core::event::{DlqPolicy, DlqSink};
use ranvier_core::{Bus, Outcome, Transition};
use ranvier_runtime::persistence::*;
use ranvier_runtime::{Axon, EffectBoundary, ExecutionReport, PersistenceOperation};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

// ---------------------------------------------------------------- fixtures

/// Counts invocations, then waits forever: the effect started and never
/// returned a result.
#[derive(Clone)]
struct StartThenHang(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Transition<u32, u32> for StartThenHang {
    type Error = String;
    type Resources = ();

    async fn run(&self, _: u32, _: &(), _: &mut Bus) -> Outcome<u32, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        std::future::pending().await
    }
}

#[derive(Clone)]
struct Count(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Transition<u32, u32> for Count {
    type Error = String;
    type Resources = ();

    async fn run(&self, input: u32, _: &(), _: &mut Bus) -> Outcome<u32, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::Next(input + 1)
    }
}

#[derive(Clone)]
struct CountCompensation(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl CompensationHook for CountCompensation {
    async fn compensate(&self, _: CompensationContext) -> anyhow::Result<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[derive(Clone)]
struct CountDlq(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl DlqSink for CountDlq {
    async fn store_dead_letter(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: &str,
        _: &[u8],
    ) -> Result<(), String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// Delegates to an in-memory store, but the `hang_on`-th append (1-based)
/// never completes.
#[derive(Clone)]
struct HangOnAppend {
    backing: InMemoryPersistenceStore,
    appends: Arc<AtomicUsize>,
    hang_on: usize,
}

#[async_trait::async_trait]
impl PersistenceStore for HangOnAppend {
    async fn append(&self, envelope: PersistenceEnvelope) -> anyhow::Result<()> {
        if self.appends.fetch_add(1, Ordering::SeqCst) + 1 == self.hang_on {
            std::future::pending::<()>().await;
        }
        self.backing.append(envelope).await
    }
    async fn load(&self, trace_id: &str) -> anyhow::Result<Option<PersistedTrace>> {
        self.backing.load(trace_id).await
    }
    async fn resume(&self, trace_id: &str, step: u64) -> anyhow::Result<ResumeCursor> {
        self.backing.resume(trace_id, step).await
    }
    async fn complete(&self, trace_id: &str, completion: CompletionState) -> anyhow::Result<()> {
        self.backing.complete(trace_id, completion).await
    }
    async fn save_intervention(
        &self,
        trace_id: &str,
        intervention: Intervention,
    ) -> anyhow::Result<()> {
        self.backing.save_intervention(trace_id, intervention).await
    }
}

fn kinds(trace: &PersistedTrace) -> Vec<&str> {
    trace
        .events
        .iter()
        .map(|event| event.outcome_kind.as_str())
        .collect()
}

// ---------------------------------------------------------------- E2

// X3: a timeout while the effect runs leaves its result unrecorded. The
// execution stops like a lost result acknowledgement: no DLQ, compensation
// or terminal history, and the next invocation finds the open intent.
// (Before P2-5 the timeout Fault was handled like a business fault.)
#[tokio::test(start_paused = true)]
async fn timeout_during_effect_is_unconfirmed_result_without_cleanup() {
    for compensated in [false, true] {
        let trace_id = format!("timeout-effect-{compensated}");
        let store = InMemoryPersistenceStore::new();
        let effects = Arc::new(AtomicUsize::new(0));
        let hooks = Arc::new(AtomicUsize::new(0));
        let dead_letters = Arc::new(AtomicUsize::new(0));
        let mut bus = Bus::new();
        bus.insert(PersistenceTraceId::new(trace_id.clone()));
        bus.insert(PersistenceHandle::from_store(store.clone()));
        bus.insert(CompensationHandle::from_hook(CountCompensation(
            hooks.clone(),
        )));
        let axon = Axon::<u32, u32, String, ()>::new("timeout-effect")
            .with_dlq_policy(DlqPolicy::SendToDlq)
            .with_dlq_sink(CountDlq(dead_letters.clone()))
            .with_saga_policy(if compensated {
                ranvier_core::saga::SagaPolicy::Enabled
            } else {
                ranvier_core::saga::SagaPolicy::Disabled
            })
            .then_with_timeout(
                StartThenHang(effects.clone()),
                Duration::from_millis(50),
                || "timeout".to_string(),
            );

        let report = axon.execute(1, &(), &mut bus).await;

        assert!(
            matches!(
                &report,
                ExecutionReport::Unconfirmed {
                    result: Outcome::Fault(message),
                    operation: PersistenceOperation::Result,
                    effect: EffectBoundary::MayHaveRun,
                    ..
                } if message == "timeout"
            ),
            "{report:?}"
        );
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        assert_eq!(hooks.load(Ordering::SeqCst), 0);
        assert_eq!(dead_letters.load(Ordering::SeqCst), 0);
        let trace = store.load(&trace_id).await.unwrap().unwrap();
        assert_eq!(kinds(&trace), vec!["Enter", "Intent"]);
        assert_eq!(trace.completion, None);

        // Re-entry finds the open intent and does not repeat the effect.
        let again = axon.execute(1, &(), &mut bus).await;
        assert!(
            matches!(
                &again,
                ExecutionReport::BlockedBeforeEffect {
                    operation: PersistenceOperation::PendingIntent,
                    ..
                }
            ),
            "{again:?}"
        );
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        assert_eq!(store.load(&trace_id).await.unwrap().unwrap(), trace);
    }
}

// X3 without a trace store: nothing is journaled, so the timeout stays an
// ordinary Fault and the report says the run was unpersisted.
#[tokio::test(start_paused = true)]
async fn timeout_without_trace_store_stays_an_unpersisted_fault() {
    let effects = Arc::new(AtomicUsize::new(0));
    let axon = Axon::<u32, u32, String, ()>::new("timeout-unpersisted").then_with_timeout(
        StartThenHang(effects.clone()),
        Duration::from_millis(50),
        || "timeout".to_string(),
    );
    let report = axon.execute(1, &(), &mut Bus::new()).await;
    assert!(
        matches!(
            &report,
            ExecutionReport::Unpersisted {
                result: Outcome::Fault(message),
                ..
            } if message == "timeout"
        ),
        "{report:?}"
    );
    assert_eq!(effects.load(Ordering::SeqCst), 1);
}

// X4: a timeout while the intent append is still pending blocks before the
// effect.
#[tokio::test(start_paused = true)]
async fn timeout_during_intent_append_blocks_before_effect() {
    let backing = InMemoryPersistenceStore::new();
    let effects = Arc::new(AtomicUsize::new(0));
    let mut bus = Bus::new();
    bus.insert(PersistenceTraceId::new("timeout-intent"));
    // Append 1 is Enter, append 2 is the effect intent.
    bus.insert(PersistenceHandle::from_store(HangOnAppend {
        backing: backing.clone(),
        appends: Arc::new(AtomicUsize::new(0)),
        hang_on: 2,
    }));
    let axon = Axon::<u32, u32, String, ()>::new("timeout-intent").then_with_timeout(
        Count(effects.clone()),
        Duration::from_millis(50),
        || "timeout".to_string(),
    );

    let report = axon.execute(1, &(), &mut bus).await;

    assert!(
        matches!(
            &report,
            ExecutionReport::BlockedBeforeEffect {
                operation: PersistenceOperation::Intent,
                effect: EffectBoundary::NotInvoked,
                ..
            }
        ),
        "{report:?}"
    );
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    let trace = backing.load("timeout-intent").await.unwrap().unwrap();
    assert_eq!(kinds(&trace), vec!["Enter"]);
}

// ---------------------------------------------------------------- E3

#[derive(Clone)]
struct HangCompensation(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl CompensationHook for HangCompensation {
    async fn compensate(&self, _: CompensationContext) -> anyhow::Result<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        std::future::pending().await
    }
}

#[derive(Clone)]
struct CountFault(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Transition<u32, u32> for CountFault {
    type Error = String;
    type Resources = ();

    async fn run(&self, _: u32, _: &(), _: &mut Bus) -> Outcome<u32, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::Fault("business-fault".into())
    }
}

/// Saga undo that starts and never returns.
#[derive(Clone)]
struct HangUndo(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Transition<u32, ()> for HangUndo {
    type Error = String;
    type Resources = ();

    async fn run(&self, _: u32, _: &(), _: &mut Bus) -> Outcome<(), String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        std::future::pending().await
    }
}

const BUDGET: Duration = Duration::from_secs(2);

fn assert_returned_within_budget(started: tokio::time::Instant) {
    let elapsed = started.elapsed();
    assert!(elapsed >= BUDGET, "returned before the budget: {elapsed:?}");
    assert!(
        elapsed < BUDGET + Duration::from_millis(100),
        "cleanup overran its budget: {elapsed:?}"
    );
}

fn has_terminal(trace: &PersistedTrace) -> bool {
    trace
        .events
        .iter()
        .any(|event| event.event_type == ranvier_runtime::PersistenceEventType::Terminal)
}

// X10 (fault path, external compensation): a hook that never returns is
// dropped when the budget closes. The compensation intent stays open and no
// terminal or completion is written.
#[tokio::test(start_paused = true)]
async fn cleanup_budget_bounds_external_compensation_after_fault() {
    let store = InMemoryPersistenceStore::new();
    let effects = Arc::new(AtomicUsize::new(0));
    let hooks = Arc::new(AtomicUsize::new(0));
    let mut bus = Bus::new();
    bus.insert(PersistenceTraceId::new("budget-fault-hook"));
    bus.insert(PersistenceHandle::from_store(store.clone()));
    bus.insert(CompensationHandle::from_hook(HangCompensation(
        hooks.clone(),
    )));
    bus.insert(CleanupBudget(BUDGET));
    let axon =
        Axon::<u32, u32, String, ()>::new("budget-fault-hook").then(CountFault(effects.clone()));

    let started = tokio::time::Instant::now();
    let report = axon.execute(1, &(), &mut bus).await;
    assert_returned_within_budget(started);

    assert!(
        matches!(
            &report,
            ExecutionReport::Unconfirmed {
                result: Outcome::Fault(message),
                operation: PersistenceOperation::PendingIntent,
                effect: EffectBoundary::MayHaveRun,
                ..
            } if message == "business-fault"
        ),
        "{report:?}"
    );
    let cleanup = report.cleanup().expect("cleanup is reported");
    assert!(!cleanup.is_complete());
    assert_eq!(
        cleanup.steps.last().unwrap().outcome,
        ranvier_runtime::SagaRollbackOutcome::InProgress
    );
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(hooks.load(Ordering::SeqCst), 1);
    let trace = store.load("budget-fault-hook").await.unwrap().unwrap();
    assert_eq!(kinds(&trace), vec!["Enter", "Intent", "Fault", "Intent"]);
    assert_eq!(trace.completion, None);
}

// X10 (fault path, Saga rollback): the window covers the undo handler too.
#[tokio::test(start_paused = true)]
async fn cleanup_budget_bounds_saga_rollback_after_fault() {
    let store = InMemoryPersistenceStore::new();
    let effects = Arc::new(AtomicUsize::new(0));
    let undos = Arc::new(AtomicUsize::new(0));
    let mut bus = Bus::new();
    bus.insert(PersistenceTraceId::new("budget-fault-saga"));
    bus.insert(PersistenceHandle::from_store(store.clone()));
    bus.insert(CleanupBudget(BUDGET));
    let axon = Axon::<u32, u32, String, ()>::new("budget-fault-saga")
        .with_saga_policy(ranvier_core::saga::SagaPolicy::Enabled)
        .then_compensated(Count(effects.clone()), HangUndo(undos.clone()))
        .then(CountFault(effects.clone()));

    let started = tokio::time::Instant::now();
    let report = axon.execute(1, &(), &mut bus).await;
    assert_returned_within_budget(started);

    assert!(
        matches!(&report, ExecutionReport::Unconfirmed { .. }),
        "{report:?}"
    );
    let cleanup = report.cleanup().expect("cleanup is reported");
    assert!(!cleanup.is_complete());
    assert!(
        cleanup
            .steps
            .iter()
            .any(|step| step.outcome == ranvier_runtime::SagaRollbackOutcome::InProgress),
        "{cleanup:?}"
    );
    assert_eq!(effects.load(Ordering::SeqCst), 2);
    assert_eq!(undos.load(Ordering::SeqCst), 1);
    let trace = store.load("budget-fault-saga").await.unwrap().unwrap();
    assert_eq!(trace.completion, None);
    assert!(!has_terminal(&trace));
}

// X10 (cancellation path): cleanup after cancellation uses the same window.
#[tokio::test(start_paused = true)]
async fn cleanup_budget_bounds_cancellation_cleanup() {
    let store = InMemoryPersistenceStore::new();
    let effects = Arc::new(AtomicUsize::new(0));
    let hooks = Arc::new(AtomicUsize::new(0));
    let mut bus = Bus::new();
    bus.insert(PersistenceTraceId::new("budget-cancel"));
    bus.insert(PersistenceHandle::from_store(store.clone()));
    bus.insert(CompensationHandle::from_hook(HangCompensation(
        hooks.clone(),
    )));
    bus.insert(CleanupBudget(BUDGET));
    let axon = Axon::<u32, u32, String, ()>::new("budget-cancel").then(Count(effects.clone()));
    let token = ranvier_core::cancellation::CancellationToken::new();
    token.cancel(ranvier_core::cancellation::CancellationReason::Explicit);

    let started = tokio::time::Instant::now();
    let report = axon.execute_cancellable(1, &(), &mut bus, token).await;
    assert_returned_within_budget(started);

    assert!(
        matches!(
            &report,
            ExecutionReport::Unconfirmed {
                result: ranvier_runtime::ExecutionTerminal::Cancelled(_),
                effect: EffectBoundary::MayHaveRun,
                ..
            }
        ),
        "{report:?}"
    );
    let cleanup = report.cleanup().expect("cleanup is reported");
    assert_eq!(
        cleanup.steps.last().unwrap().outcome,
        ranvier_runtime::SagaRollbackOutcome::InProgress
    );
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert_eq!(hooks.load(Ordering::SeqCst), 1);
    let trace = store.load("budget-cancel").await.unwrap().unwrap();
    assert_eq!(trace.completion, None);
    assert!(!has_terminal(&trace));
}

// A budget that has already elapsed when cleanup starts invokes nothing.
#[tokio::test(start_paused = true)]
async fn zero_cleanup_budget_invokes_no_handler() {
    let hooks = Arc::new(AtomicUsize::new(0));
    let mut bus = Bus::new();
    bus.insert(PersistenceTraceId::new("budget-zero"));
    bus.insert(PersistenceHandle::from_store(
        InMemoryPersistenceStore::new(),
    ));
    bus.insert(CompensationHandle::from_hook(HangCompensation(
        hooks.clone(),
    )));
    bus.insert(CleanupBudget(Duration::ZERO));
    let axon = Axon::<u32, u32, String, ()>::new("budget-zero")
        .then(CountFault(Arc::new(AtomicUsize::new(0))));
    let report = axon.execute(1, &(), &mut bus).await;
    let cleanup = report.cleanup().expect("cleanup is reported");
    assert_eq!(
        cleanup.steps.last().unwrap().outcome,
        ranvier_runtime::SagaRollbackOutcome::NotInvoked
    );
    assert_eq!(hooks.load(Ordering::SeqCst), 0);
}

// ---------------------------------------------------------------- E4

/// Faults for the first `faults` invocations, then succeeds.
#[derive(Clone)]
struct FaultThenSucceed {
    calls: Arc<AtomicUsize>,
    faults: usize,
}

#[async_trait::async_trait]
impl Transition<u32, u32> for FaultThenSucceed {
    type Error = String;
    type Resources = ();

    async fn run(&self, input: u32, _: &(), _: &mut Bus) -> Outcome<u32, String> {
        if self.calls.fetch_add(1, Ordering::SeqCst) < self.faults {
            Outcome::Fault("transient".into())
        } else {
            Outcome::Next(input + 1)
        }
    }
}

/// Delegates to an in-memory store. The append selected by `fail` is applied
/// (when `apply` is set) and then reported as failed, like a lost
/// acknowledgement. `after_ok` runs after every acknowledged append.
#[derive(Clone)]
struct FaultyStore {
    backing: InMemoryPersistenceStore,
    fail: Arc<dyn Fn(&PersistenceEnvelope) -> bool + Send + Sync>,
    apply: bool,
    after_ok: Arc<dyn Fn(&PersistenceEnvelope) + Send + Sync>,
}

impl FaultyStore {
    fn failing(
        backing: &InMemoryPersistenceStore,
        apply: bool,
        fail: impl Fn(&PersistenceEnvelope) -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            backing: backing.clone(),
            fail: Arc::new(fail),
            apply,
            after_ok: Arc::new(|_| {}),
        }
    }
}

#[async_trait::async_trait]
impl PersistenceStore for FaultyStore {
    async fn append(&self, envelope: PersistenceEnvelope) -> anyhow::Result<()> {
        if (self.fail)(&envelope) {
            if self.apply {
                self.backing.append(envelope).await?;
            }
            anyhow::bail!("injected append failure");
        }
        self.backing.append(envelope.clone()).await?;
        (self.after_ok)(&envelope);
        // Yield like a network store, so a cancellation raised in `after_ok`
        // is observed while this call is still in flight.
        tokio::task::yield_now().await;
        Ok(())
    }
    async fn load(&self, trace_id: &str) -> anyhow::Result<Option<PersistedTrace>> {
        self.backing.load(trace_id).await
    }
    async fn resume(&self, trace_id: &str, step: u64) -> anyhow::Result<ResumeCursor> {
        self.backing.resume(trace_id, step).await
    }
    async fn complete(&self, trace_id: &str, completion: CompletionState) -> anyhow::Result<()> {
        self.backing.complete(trace_id, completion).await
    }
    async fn save_intervention(
        &self,
        trace_id: &str,
        intervention: Intervention,
    ) -> anyhow::Result<()> {
        self.backing.save_intervention(trace_id, intervention).await
    }
}

fn count_type(trace: &PersistedTrace, kind: ranvier_runtime::PersistenceEventType) -> usize {
    trace
        .events
        .iter()
        .filter(|event| event.event_type == kind)
        .count()
}

// X5: `RetryThenDlq` runs every attempt under one intent and records only
// the final outcome. Intermediate faults are not journaled.
#[tokio::test(start_paused = true)]
async fn retry_then_dlq_journals_one_intent_and_one_result() {
    use ranvier_runtime::PersistenceEventType as T;
    for (faults, expected_kinds, dlq, completion) in [
        (
            2,
            vec!["Enter", "Intent", "Next", "Terminal"],
            0,
            CompletionState::Success,
        ),
        (
            usize::MAX,
            vec!["Enter", "Intent", "Fault", "Terminal"],
            1,
            CompletionState::Fault,
        ),
    ] {
        let trace_id = format!("retry-dlq-{faults}");
        let store = InMemoryPersistenceStore::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let dead_letters = Arc::new(AtomicUsize::new(0));
        let mut bus = Bus::new();
        bus.insert(PersistenceTraceId::new(trace_id.clone()));
        bus.insert(PersistenceHandle::from_store(store.clone()));
        bus.insert(PersistenceAutoComplete(true));
        let axon = Axon::<u32, u32, String, ()>::new("retry-dlq")
            .with_dlq_policy(DlqPolicy::RetryThenDlq {
                max_attempts: 3,
                backoff_ms: 10,
            })
            .with_dlq_sink(CountDlq(dead_letters.clone()))
            .then(FaultThenSucceed {
                calls: calls.clone(),
                faults,
            });

        let report = axon.execute(1, &(), &mut bus).await;

        assert!(
            matches!(&report, ExecutionReport::Recorded { .. }),
            "{report:?}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(dead_letters.load(Ordering::SeqCst), dlq);
        let trace = store.load(&trace_id).await.unwrap().unwrap();
        let mut actual: Vec<&str> = kinds(&trace);
        // The terminal event's domain kind mirrors the outcome; compare roles.
        if let Some(last) = trace.events.last()
            && last.event_type == T::Terminal
        {
            *actual.last_mut().unwrap() = "Terminal";
        }
        assert_eq!(actual, expected_kinds);
        assert_eq!(count_type(&trace, T::Intent), 1);
        assert_eq!(trace.completion, Some(completion));
    }
}

// X6: `then_with_retry` journals every attempt as its own intent and result.
// A lost result acknowledgement stops the remaining attempts.
#[tokio::test(start_paused = true)]
async fn explicit_retry_journals_each_attempt_and_stops_after_lost_ack() {
    use ranvier_runtime::PersistenceEventType as T;
    let policy = ranvier_runtime::RetryPolicy::fixed(3, Duration::from_millis(10));

    // Fault, then success: two intent/result pairs.
    let store = InMemoryPersistenceStore::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut bus = Bus::new();
    bus.insert(PersistenceTraceId::new("explicit-retry"));
    bus.insert(PersistenceHandle::from_store(store.clone()));
    let axon = Axon::<u32, u32, String, ()>::new("explicit-retry").then_with_retry(
        FaultThenSucceed {
            calls: calls.clone(),
            faults: 1,
        },
        policy.clone(),
    );
    let report = axon.execute(1, &(), &mut bus).await;
    assert!(
        matches!(
            &report,
            ExecutionReport::Recorded {
                result: Outcome::Next(2),
                ..
            } | ExecutionReport::NotRequested {
                result: Outcome::Next(2),
                ..
            }
        ),
        "{report:?}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let trace = store.load("explicit-retry").await.unwrap().unwrap();
    assert_eq!(count_type(&trace, T::Intent), 2);
    assert_eq!(
        &kinds(&trace)[..5],
        ["Enter", "Intent", "Fault", "Intent", "Next"]
    );

    // The second attempt's result acknowledgement is lost after it applied:
    // no third attempt.
    for apply in [false, true] {
        let trace_id = format!("explicit-retry-lost-{apply}");
        let backing = InMemoryPersistenceStore::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let results = Arc::new(AtomicUsize::new(0));
        let mut bus = Bus::new();
        bus.insert(PersistenceTraceId::new(trace_id.clone()));
        let seen = results.clone();
        bus.insert(PersistenceHandle::from_store(FaultyStore::failing(
            &backing,
            apply,
            move |envelope| {
                envelope.event_type == T::Result && seen.fetch_add(1, Ordering::SeqCst) == 1
            },
        )));
        let axon = Axon::<u32, u32, String, ()>::new("explicit-retry").then_with_retry(
            FaultThenSucceed {
                calls: calls.clone(),
                faults: usize::MAX,
            },
            policy.clone(),
        );
        let report = axon.execute(1, &(), &mut bus).await;
        assert!(
            matches!(
                &report,
                ExecutionReport::Unconfirmed {
                    operation: PersistenceOperation::Result,
                    effect: EffectBoundary::MayHaveRun,
                    ..
                }
            ),
            "{report:?}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let trace = backing.load(&trace_id).await.unwrap().unwrap();
        assert_eq!(count_type(&trace, T::Intent), 2);
        assert_eq!(count_type(&trace, T::Terminal), 0);
        assert_eq!(trace.completion, None);
    }
}

// X8: hook and marker succeed, then the "Compensated" event append fails.
// The execution is unconfirmed; a later run does not call the hook again.
#[tokio::test]
async fn compensated_event_failure_after_hook_does_not_repeat_hook() {
    for apply in [false, true] {
        let trace_id = format!("compensated-event-{apply}");
        let backing = InMemoryPersistenceStore::new();
        let markers = InMemoryCompensationIdempotencyStore::new();
        let effects = Arc::new(AtomicUsize::new(0));
        let hooks = Arc::new(AtomicUsize::new(0));
        let mut bus = Bus::new();
        bus.insert(PersistenceTraceId::new(trace_id.clone()));
        bus.insert(PersistenceHandle::from_store(FaultyStore::failing(
            &backing,
            apply,
            |envelope| envelope.outcome_kind == "Compensated",
        )));
        bus.insert(CompensationIdempotencyHandle::from_store(markers.clone()));
        bus.insert(CompensationHandle::from_hook(CountCompensation(
            hooks.clone(),
        )));
        let axon = Axon::<u32, u32, String, ()>::new("compensated-event")
            .then(CountFault(effects.clone()));

        let report = axon.execute(1, &(), &mut bus).await;
        assert!(
            matches!(
                &report,
                ExecutionReport::Unconfirmed {
                    result: Outcome::Fault(_),
                    operation: PersistenceOperation::CompensationEvent,
                    effect: EffectBoundary::MayHaveRun,
                    ..
                }
            ),
            "{report:?}"
        );
        assert_eq!(hooks.load(Ordering::SeqCst), 1);
        let trace = backing.load(&trace_id).await.unwrap().unwrap();
        assert_eq!(trace.completion, None);
        assert!(!has_terminal(&trace));

        let again = axon.execute(1, &(), &mut bus).await;
        assert!(
            !matches!(&again, ExecutionReport::Recorded { .. }),
            "{again:?}"
        );
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        assert_eq!(hooks.load(Ordering::SeqCst), 1);
    }
}

// X12: cancellation that arrives just as the result is acknowledged. The
// store applied the result, but the execution was cancelled before the call
// returned, so the result is unconfirmed. The trace stays consistent: the
// first effect ran once, the next step never started, and the trace can be
// continued.
#[tokio::test]
async fn cancellation_at_result_ack_leaves_a_resumable_trace() {
    use ranvier_runtime::PersistenceEventType as T;
    let backing = InMemoryPersistenceStore::new();
    let first = Arc::new(AtomicUsize::new(0));
    let second = Arc::new(AtomicUsize::new(0));
    let token = ranvier_core::cancellation::CancellationToken::new();
    let cancel = token.clone();
    let mut bus = Bus::new();
    bus.insert(PersistenceTraceId::new("cancel-at-result"));
    bus.insert(PersistenceHandle::from_store(FaultyStore {
        backing: backing.clone(),
        fail: Arc::new(|_| false),
        apply: false,
        after_ok: Arc::new(move |envelope| {
            if envelope.event_type == T::Result {
                cancel.cancel(ranvier_core::cancellation::CancellationReason::Explicit);
            }
        }),
    }));
    let axon = Axon::<u32, u32, String, ()>::new("cancel-at-result")
        .then(Count(first.clone()))
        .then(Count(second.clone()));

    let report = axon.execute_cancellable(1, &(), &mut bus, token).await;

    assert!(
        matches!(
            &report,
            ExecutionReport::Unconfirmed {
                result: ranvier_runtime::ExecutionTerminal::Cancelled(_),
                operation: PersistenceOperation::Result,
                effect: EffectBoundary::MayHaveRun,
                ..
            }
        ),
        "{report:?}"
    );
    assert_eq!(
        (first.load(Ordering::SeqCst), second.load(Ordering::SeqCst)),
        (1, 0)
    );
    let trace = backing.load("cancel-at-result").await.unwrap().unwrap();
    assert_eq!(count_type(&trace, T::Result), 1);
    assert_eq!(trace.reconciliation(), TraceReconciliation::Open);

    // Continuing the trace runs only the second step.
    let mut bus = Bus::new();
    bus.insert(PersistenceTraceId::new("cancel-at-result"));
    bus.insert(PersistenceHandle::from_store(backing.clone()));
    let report = axon.execute(1, &(), &mut bus).await;
    assert!(
        matches!(
            &report,
            ExecutionReport::Recorded {
                result: Outcome::Next(3),
                ..
            }
        ),
        "{report:?}"
    );
    assert_eq!(
        (first.load(Ordering::SeqCst), second.load(Ordering::SeqCst)),
        (1, 1)
    );
}

// Regression found by P2-5: `then_with_timeout` and `then_with_retry` steps
// did not honour resumption, so continuing a trace re-ran earlier steps.
// The resumed step now starts from its stored checkpoint.
#[tokio::test(start_paused = true)]
async fn resumed_trace_does_not_rerun_timeout_or_retry_steps() {
    use ranvier_runtime::PersistenceEventType as T;
    for retry in [false, true] {
        let trace_id = format!("resume-wrapped-{retry}");
        let backing = InMemoryPersistenceStore::new();
        let first = Arc::new(AtomicUsize::new(0));
        let second = Arc::new(AtomicUsize::new(0));
        let build = |first: &Arc<AtomicUsize>, second: &Arc<AtomicUsize>| {
            let axon = Axon::<u32, u32, String, ()>::new("resume-wrapped");
            if retry {
                let policy = ranvier_runtime::RetryPolicy::fixed(1, Duration::from_millis(1));
                axon.then_with_retry(Count(first.clone()), policy.clone())
                    .then_with_retry(Count(second.clone()), policy)
            } else {
                axon.then_with_timeout(Count(first.clone()), Duration::from_secs(1), || {
                    "timeout".to_string()
                })
                .then_with_timeout(
                    Count(second.clone()),
                    Duration::from_secs(1),
                    || "timeout".to_string(),
                )
            }
        };

        // First run: the second step's intent is not written.
        let intents = Arc::new(AtomicUsize::new(0));
        let mut bus = Bus::new();
        bus.insert(PersistenceTraceId::new(trace_id.clone()));
        let seen = intents.clone();
        bus.insert(PersistenceHandle::from_store(FaultyStore::failing(
            &backing,
            false,
            move |envelope| {
                envelope.event_type == T::Intent && seen.fetch_add(1, Ordering::SeqCst) == 1
            },
        )));
        let report = build(&first, &second).execute(1, &(), &mut bus).await;
        assert!(
            matches!(
                &report,
                ExecutionReport::Unconfirmed { .. } | ExecutionReport::BlockedBeforeEffect { .. }
            ),
            "{report:?}"
        );
        assert_eq!(
            (first.load(Ordering::SeqCst), second.load(Ordering::SeqCst)),
            (1, 0)
        );

        // Second run continues at the second step with the stored checkpoint.
        let mut bus = Bus::new();
        bus.insert(PersistenceTraceId::new(trace_id.clone()));
        bus.insert(PersistenceHandle::from_store(backing.clone()));
        let report = build(&first, &second).execute(1, &(), &mut bus).await;
        assert!(
            matches!(
                &report,
                ExecutionReport::Recorded {
                    result: Outcome::Next(3),
                    ..
                }
            ),
            "{report:?}"
        );
        assert_eq!(
            (first.load(Ordering::SeqCst), second.load(Ordering::SeqCst)),
            (1, 1)
        );
    }
}
