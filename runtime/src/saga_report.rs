//! Per-invocation Saga rollback observations, available on the caller's Bus.
//! Hook completion does not establish external effect reversal or durability.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SagaRollbackOutcome {
    /// No handler invocation was observed for this retained input, including
    /// budget exhaustion or scope restoration before rollback reached it.
    NotInvoked,
    InProgress,
    Panicked,
    Completed,
    Fault,
    Emitted,
    Branched,
    Jumped,
    MissingHandler,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SagaRollbackStep {
    pub node_id: String,
    pub outcome: SagaRollbackOutcome,
}

/// Ordered hook observations. No raw handler error or snapshot is included.
/// This is a local cleanup report, not a stored checkpoint or retry permission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SagaRollbackReport {
    pub trace_id: String,
    /// False until every queued rollback task has been inspected.
    pub finished: bool,
    pub steps: Vec<SagaRollbackStep>,
}

impl SagaRollbackReport {
    /// True if every requested hook returned Next(()); an empty rollback has
    /// no incomplete hooks. This does not certify any external effect reversal.
    pub fn is_complete(&self) -> bool {
        self.finished
            && self
                .steps
                .iter()
                .all(|step| step.outcome == SagaRollbackOutcome::Completed)
    }
}

/// Sensitive local recovery capability; no automatic retry or serialization.
#[derive(Clone)]
pub(crate) struct ExternalCompensationRecovery {
    pub(crate) context: crate::persistence::CompensationContext,
    pub(crate) handle: crate::persistence::CompensationHandle,
    pub(crate) idempotency: Option<crate::persistence::CompensationIdempotencyHandle>,
    pub(crate) invoked: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
/// Invocation-owned cleanup journal. Cloning this handle does not copy steps.
/// Call snapshot to obtain an owned serializable report, including partial work.
#[derive(Clone)]
pub struct SagaRollbackJournal(
    std::sync::Arc<std::sync::Mutex<SagaRollbackReport>>,
    std::sync::Arc<std::sync::Mutex<Vec<(usize, std::sync::Arc<ranvier_core::saga::SagaTask>)>>>,
    std::sync::Arc<std::sync::Mutex<Vec<(usize, ExternalCompensationRecovery)>>>,
    std::sync::Arc<std::sync::Mutex<Vec<(usize, SagaRollbackJournal)>>>,
    std::sync::Arc<std::sync::Mutex<Option<crate::nested_custody::WeakCustody>>>,
    #[cfg(feature = "streaming")]
    std::sync::Arc<std::sync::Mutex<Vec<(usize, tokio::sync::watch::Receiver<crate::streaming_recovery::SupervisorObservation>)>>>,
);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JournalRetentionError { Unavailable, TraversalLimit }
impl std::fmt::Debug for SagaRollbackJournal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SagaRollbackJournal")
            .field("report", &self.snapshot())
            .finish()
    }
}

impl SagaRollbackJournal {
    pub(crate) fn new(trace_id: &str) -> Self {
        Self(
            std::sync::Arc::new(std::sync::Mutex::new(SagaRollbackReport {
                trace_id: trace_id.to_string(),
                finished: false,
                steps: Vec::new(),
            })),
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            std::sync::Arc::new(std::sync::Mutex::new(None)),
            #[cfg(feature = "streaming")]
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        )
    }
    /// Attach safe child observation at an actual parent-local report index.
    /// A returned stream is not automatically reconciled or removed: even EOF may
    /// leave successful child effects requiring compensation after parent failure.
    #[cfg(feature = "streaming")]
    pub(crate) fn retain_stream_child(
        &self,
        observer: tokio::sync::watch::Receiver<crate::streaming_recovery::SupervisorObservation>,
    ) {
        let mut report = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let index = report.steps.len();
        report.finished = false;
        report.steps.push(SagaRollbackStep {
            node_id: "nested-stream-invocation".into(),
            outcome: SagaRollbackOutcome::InProgress,
        });
        self.5.lock().unwrap_or_else(|p| p.into_inner()).push((index, observer));
    }
    #[cfg(feature = "streaming")]
    pub(crate) fn retained_stream_observations(&self) -> Vec<(usize, crate::streaming_recovery::SupervisorObservation)> {
        self.5.lock().unwrap_or_else(|p| p.into_inner()).iter()
            .map(|(index, observer)| (*index, observer.borrow().clone())).collect()
    }
    /// Wait for the current child observation set using one absolute deadline.
    /// This does not cancel children or retire their recovery records. Returned
    /// and Interrupted are local execution termination, not durable completion.
    #[cfg(feature = "streaming")]
    pub(crate) async fn drain_stream_children(&self, deadline: tokio::time::Instant) -> Result<(), usize> {
        use futures_util::{stream::FuturesUnordered, StreamExt};
        let observers: Vec<_> = self.5.lock().unwrap_or_else(|p| p.into_inner())
            .iter().map(|(_, observer)| observer.clone()).collect();
        let mut pending = FuturesUnordered::new();
        for mut observer in observers {
            pending.push(async move {
                loop {
                    let settled = {
                        let state = observer.borrow_and_update();
                        matches!(state.phase, crate::streaming_recovery::SupervisorPhase::Returned
                            | crate::streaming_recovery::SupervisorPhase::Interrupted)
                            || state.custody != crate::streaming_recovery::RecoveryCustody::Held
                    };
                    if settled { return true; }
                    if observer.changed().await.is_err() { return false; }
                }
            });
        }
        let mut unknown = 0;
        while !pending.is_empty() {
            match tokio::time::timeout_at(deadline, pending.next()).await {
                Ok(Some(true)) => {},
                Ok(Some(false)) => unknown += 1,
                Ok(None) => break,
                Err(_) => return Err(unknown + pending.len()),
            }
        }
        if unknown == 0 { Ok(()) } else { Err(unknown) }
    }
    pub(crate) fn retain_custody(&self, custody: crate::nested_custody::WeakCustody) {
        let mut incoming = Some(custody);
        {
            let mut retained = self.4.lock().unwrap_or_else(|p| p.into_inner());
            if retained.is_none() { *retained = incoming.take(); }
        }
        // Any user resource destructor must run outside the journal lock.
        if let Some(incoming) = incoming { incoming.release(); }
    }
    pub(crate) fn release_custody(&self) {
        let released = self.4.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(released) = released { released.release(); }
    }
    #[cfg(test)]
    pub(crate) fn retained_custody(&self) -> Option<std::sync::Arc<dyn std::any::Any + Send + Sync>> {
        self.4.lock().unwrap_or_else(|p| p.into_inner()).as_ref().and_then(|record| record.upgrade())
    }
    #[cfg(all(test, feature = "streaming"))]
    pub(crate) fn poison_retention_for_test(&self) {
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _held = self.1.lock().unwrap(); panic!("retention poison fixture");
        })).is_err());
    }
    /// Owned local reconciliation inputs. These may contain secrets: never expose
    /// them as public responses/logs. Retention does not authorize replay or prove
    /// durable recovery; typed resources and graph handler ownership are still needed.
    pub fn unresolved_tasks(&self) -> Vec<ranvier_core::saga::SagaTask> {
        self.1
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .map(|(_, task)| task.as_ref().clone())
            .collect()
    }
    /// Bounded, fail-closed live check. Never clones retained input bytes and
    /// never waits on a synchronous journal mutex held by another operation.
    #[cfg(feature = "streaming")]
    pub(crate) fn try_recovery_pending(&self, limit: usize) -> Result<bool, crate::streaming_report_handle::StreamRecoveryProbeError> {
        use crate::streaming_report_handle::StreamRecoveryProbeError as Error;
        let mut pending = vec![self.clone()];
        let mut seen = std::collections::HashSet::new();
        let mut work = 0usize;
        while let Some(journal) = pending.pop() {
            if !seen.insert(std::sync::Arc::as_ptr(&journal.0) as usize) { return Err(Error::TraversalLimit); }
            let report = journal.0.try_lock().map_err(|_| Error::Unavailable)?;
            work = work.saturating_add(1).saturating_add(report.steps.len());
            if work > limit { return Err(Error::TraversalLimit); }
            let incomplete = !report.is_complete();
            drop(report);
            if incomplete || !journal.1.try_lock().map_err(|_| Error::Unavailable)?.is_empty()
                || !journal.2.try_lock().map_err(|_| Error::Unavailable)?.is_empty()
                || !journal.5.try_lock().map_err(|_| Error::Unavailable)?.is_empty() {
                return Ok(true);
            }
            let children = journal.3.try_lock().map_err(|_| Error::Unavailable)?;
            if pending.len().saturating_add(children.len()) > limit { return Err(Error::TraversalLimit); }
            pending.extend(children.iter().map(|(_, child)| child.clone()));
        }
        Ok(false)
    }
    /// Presence of retained work, not hook completion or retirement permission.
    /// Nonblocking and bounded even during scope restoration or final owner Drop.
    /// Unknown state must retain custody rather than certify an empty journal.
    pub(crate) fn try_retained_work_pending(&self, limit: usize) -> Result<bool, JournalRetentionError> {
        let mut pending = vec![self.clone()];
        let mut seen = std::collections::HashSet::new();
        while let Some(journal) = pending.pop() {
            if seen.len() >= limit || !seen.insert(std::sync::Arc::as_ptr(&journal.0) as usize) {
                return Err(JournalRetentionError::TraversalLimit);
            }
            if !journal.1.try_lock().map_err(|_| JournalRetentionError::Unavailable)?.is_empty()
                || !journal.2.try_lock().map_err(|_| JournalRetentionError::Unavailable)?.is_empty() {
                return Ok(true);
            }
            #[cfg(feature = "streaming")]
            if !journal.5.try_lock().map_err(|_| JournalRetentionError::Unavailable)?.is_empty() {
                return Ok(true);
            }
            let children = journal.3.try_lock().map_err(|_| JournalRetentionError::Unavailable)?;
            if seen.len().saturating_add(pending.len()).saturating_add(children.len()) > limit {
                return Err(JournalRetentionError::TraversalLimit);
            }
            // Only bounded journal handles are cloned; retained inputs never are.
            pending.extend(children.iter().map(|(_, child)| child.clone()));
        }
        Ok(false)
    }
    /// Retention callers treat any unknown state as unresolved.
    /// This is not report completeness, reconciliation or replay authority.
    pub(crate) fn has_unresolved_tasks(&self) -> bool {
        self.try_retained_work_pending(256).unwrap_or(true)
    }
    /// Session attaches only a newly isolated invocation journal to its suspended
    /// parent. Keeping child storage distinct preserves local indices and trace.
    /// This is custody, not replay authority or typed child-resource recovery.
    pub(crate) fn retain_child(&self, child: Self) {
        let mut report = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let index = report.steps.len();
        report.finished = false;
        report.steps.push(SagaRollbackStep {
            node_id: "nested-invocation".into(),
            outcome: SagaRollbackOutcome::InProgress,
        });
        self.3
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push((index, child));
    }
    #[cfg(all(test, feature = "streaming"))]
    pub(crate) fn shares_report_with(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
    pub(crate) fn retained_children(&self) -> Vec<(usize, Self)> {
        self.3.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
    pub(crate) fn push_uninvoked_task(&self, task: ranvier_core::saga::SagaTask) {
        self.push_task_observed(std::sync::Arc::new(task), SagaRollbackOutcome::NotInvoked);
    }
    pub(crate) fn push_task(&self, task: ranvier_core::saga::SagaTask) {
        self.push_task_shared(std::sync::Arc::new(task));
    }
    pub(crate) fn push_task_shared(&self, task: std::sync::Arc<ranvier_core::saga::SagaTask>) {
        self.push_task_observed(task, SagaRollbackOutcome::InProgress);
    }
    fn push_task_observed(&self, task: std::sync::Arc<ranvier_core::saga::SagaTask>, outcome: SagaRollbackOutcome) {
        let mut report = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let index = report.steps.len();
        report.finished = false;
        report.steps.push(SagaRollbackStep {
            node_id: task.node_id.clone(),
            outcome,
        });
        self.1
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push((index, task));
    }
    pub(crate) fn can_finish_external(&self) -> bool {
        let report = self.0.lock().unwrap_or_else(|p| p.into_inner());
        report.finished || report.steps.is_empty()
    }
    pub(crate) fn push_external(&self, recovery: ExternalCompensationRecovery) -> usize {
        let mut report = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let index = report.steps.len();
        report.finished = false;
        report.steps.push(SagaRollbackStep {
            node_id: "external-compensation".into(),
            outcome: SagaRollbackOutcome::InProgress,
        });
        self.2
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push((index, recovery));
        index
    }
    /// Sensitive exact context/capability, accessible only to runtime reconciliation.
    pub(crate) fn unresolved_external(&self) -> Vec<ExternalCompensationRecovery> {
        self.2
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .map(|(_, item)| item.clone())
            .collect()
    }
    pub(crate) fn settle_external(&self, index: usize, outcome: SagaRollbackOutcome) {
        {
            let mut report = self.0.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(step) = report.steps.get_mut(index) {
                step.outcome = outcome;
            }
        }
        // Releasing a user hook can invoke its destructor. Never do that under journal locks.
        let released = if outcome == SagaRollbackOutcome::Completed {
            let mut retained = self.2.lock().unwrap_or_else(|p| p.into_inner());
            retained
                .iter()
                .position(|(observed, _)| *observed == index)
                .map(|position| retained.swap_remove(position))
        } else {
            None
        };
        drop(released);
    }
    pub fn snapshot(&self) -> SagaRollbackReport {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
    pub(crate) fn matches_trace(&self, trace_id: &str) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .trace_id
            == trace_id
    }
    pub(crate) fn push(&self, node_id: String, outcome: SagaRollbackOutcome) {
        let mut report = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        report.finished = false;
        report.steps.push(SagaRollbackStep { node_id, outcome });
    }
    pub(crate) fn settle(&self, outcome: SagaRollbackOutcome) {
        let mut report = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(step) = report.steps.last_mut() {
            step.outcome = outcome;
        }
        if outcome == SagaRollbackOutcome::Completed {
            let index = report.steps.len().saturating_sub(1);
            self.1
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .retain(|(observed, _)| *observed != index);
        }
    }
    /// An exhausted budget cannot reuse a previous finished bit for newly queued work.
    #[cfg(feature = "streaming")]
    pub(crate) fn mark_unfinished(&self) {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).finished = false;
    }
    pub(crate) fn finish(&self) {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .finished = true;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn uninvoked_task_retention_moves_bytes_and_preserves_prior_observation() {
        let journal = super::SagaRollbackJournal::new("move-input");
        journal.push("already-entered".into(), super::SagaRollbackOutcome::InProgress);
        let bytes = vec![1, 2, 3, 4];
        let allocation = bytes.as_ptr();
        journal.push_uninvoked_task(ranvier_core::saga::SagaTask {
            node_id: "remaining".into(), node_label: "remaining".into(), input_snapshot: bytes,
        });
        let retained = journal.1.lock().unwrap();
        assert_eq!(retained[0].1.input_snapshot.as_ptr(), allocation);
        assert_eq!(retained[0].0, 1);
        drop(retained);
        let report = journal.snapshot();
        assert!(!report.finished);
        assert_eq!(report.steps[0].outcome, super::SagaRollbackOutcome::InProgress);
        assert_eq!(report.steps[1].outcome, super::SagaRollbackOutcome::NotInvoked);
    }
    use super::*;
    struct ReleaseProbe {
        report: std::sync::Weak<std::sync::Mutex<SagaRollbackReport>>,
        retained: std::sync::Weak<std::sync::Mutex<Vec<(usize, ExternalCompensationRecovery)>>>,
        released: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }
    impl Drop for ReleaseProbe {
        fn drop(&mut self) {
            assert!(self.report.upgrade().unwrap().try_lock().is_ok());
            assert!(self.retained.upgrade().unwrap().try_lock().is_ok());
            self.released
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    #[async_trait::async_trait]
    impl crate::persistence::CompensationHook for ReleaseProbe {
        async fn compensate(
            &self,
            _: crate::persistence::CompensationContext,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn completed_external_release_drops_user_hook_outside_journal_locks() {
        let journal = SagaRollbackJournal::new("release-probe");
        let released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let recovery = ExternalCompensationRecovery {
            context: crate::persistence::CompensationContext {
                trace_id: "release-probe".into(),
                circuit: "private-circuit".into(),
                fault_kind: "Fault".into(),
                fault_step: 1,
                timestamp_ms: 0,
            },
            handle: crate::persistence::CompensationHandle::from_hook(ReleaseProbe {
                report: std::sync::Arc::downgrade(&journal.0),
                retained: std::sync::Arc::downgrade(&journal.2),
                released: released.clone(),
            }),
            idempotency: None,
            invoked: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        let index = journal.push_external(recovery);
        journal.settle_external(index, SagaRollbackOutcome::Completed);
        assert!(released.load(std::sync::atomic::Ordering::SeqCst));
        assert!(journal.unresolved_external().is_empty());
    }
    #[test]
    fn unresolved_inputs_are_private_and_completion_removes_only_its_own_observation() {
        let journal = SagaRollbackJournal::new("recovery-fixture");
        let task = ranvier_core::saga::SagaTask {
            node_id: "same-node".into(),
            node_label: "private-label".into(),
            input_snapshot: b"private-snapshot".to_vec(),
        };
        journal.push_task(task.clone());
        journal.settle(SagaRollbackOutcome::MissingHandler);
        journal.push_task(task);
        journal.settle(SagaRollbackOutcome::Completed);
        journal.finish();
        assert!(journal.has_unresolved_tasks());
        assert_eq!(journal.unresolved_tasks().len(), 1);
        assert_eq!(
            journal.unresolved_tasks()[0].input_snapshot,
            b"private-snapshot"
        );
        let published = journal.clone();
        assert_eq!(published.unresolved_tasks().len(), 1);
        // Local reconciliation may mutate its copy without changing retained evidence.
        let mut local = published.unresolved_tasks();
        local[0].node_id.clear();
        local[0].node_label.clear();
        local[0].input_snapshot.fill(0);
        local.clear();
        let retained = journal.unresolved_tasks();
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].node_id, "same-node");
        assert_eq!(retained[0].node_label, "private-label");
        assert_eq!(retained[0].input_snapshot, b"private-snapshot");
        assert!(!journal.snapshot().is_complete());
        let wire = serde_json::to_string(&journal.snapshot()).unwrap();
        assert!(!wire.contains("private"));
        assert!(!format!("{journal:?}").contains("private"));
    }

    #[cfg(feature = "streaming")]
    #[test]
    fn live_probe_checks_children_limits_busy_and_poison_without_input_clones() {
        use crate::streaming_report_handle::StreamRecoveryProbeError as Error;
        let root = SagaRollbackJournal::new("private root trace"); root.finish();
        assert_eq!(root.try_recovery_pending(4), Ok(false));
        let child = SagaRollbackJournal::new("private child trace");
        root.retain_child(child.clone());
        root.0.lock().unwrap().steps[0].outcome = SagaRollbackOutcome::Completed; root.finish();
        assert_eq!(root.try_recovery_pending(4), Ok(true));
        child.finish();
        assert_eq!(root.try_recovery_pending(1), Err(Error::TraversalLimit));
        assert_eq!(root.try_recovery_pending(4), Ok(false));
        let busy = child.1.lock().unwrap();
        assert_eq!(root.try_recovery_pending(4), Err(Error::Unavailable)); drop(busy);
        let poisoned = child.clone();
        assert!(std::thread::spawn(move || { let _guard = poisoned.1.lock().unwrap(); panic!("journal probe poison fixture"); }).join().is_err());
        assert_eq!(root.try_recovery_pending(4), Err(Error::Unavailable));
    }
    #[test]
    fn long_journal_clones_share_storage_and_snapshots_are_owned() {
        let journal = SagaRollbackJournal::new("long-journal");
        let published = journal.clone();
        assert!(std::sync::Arc::ptr_eq(&journal.0, &published.0));
        for index in 0..10_000 {
            journal.push(index.to_string(), SagaRollbackOutcome::InProgress);
            journal.settle(SagaRollbackOutcome::Completed);
        }
        journal.finish();
        assert!(!published.has_unresolved_tasks());
        let snapshot = published.snapshot();
        assert_eq!(snapshot.steps.len(), 10_000);
        assert!(snapshot.is_complete());
        journal.push("later".into(), SagaRollbackOutcome::InProgress);
        assert_eq!(snapshot.steps.len(), 10_000);
        assert_eq!(published.snapshot().steps.len(), 10_001);
        assert!(!published.snapshot().is_complete());
    }
    #[test]
    fn custody_check_does_not_wait_on_busy_journal() {
        let journal = SagaRollbackJournal::new("busy custody");
        let held = journal.1.lock().unwrap();
        let clone = journal.clone();
        let (send, receive) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || { send.send(clone.has_unresolved_tasks()).unwrap(); });
        let result = receive.recv_timeout(std::time::Duration::from_millis(200));
        drop(held);
        worker.join().unwrap();
        assert_eq!(result, Ok(true));
    }
    #[test]
    fn retained_work_check_bounds_cycles_graph_size_and_poison_without_input_clones() {
        let root = SagaRollbackJournal::new("retention root");
        let child = SagaRollbackJournal::new("retention child");
        root.retain_child(child.clone());
        assert_eq!(root.try_retained_work_pending(2), Ok(false));
        assert_eq!(root.try_retained_work_pending(1), Err(JournalRetentionError::TraversalLimit));
        child.retain_child(root.clone());
        let cyclic = root.try_retained_work_pending(4);
        let retained = root.has_unresolved_tasks();
        root.3.lock().unwrap().clear(); child.3.lock().unwrap().clear();
        assert_eq!(cyclic, Err(JournalRetentionError::TraversalLimit)); assert!(retained);
        let poison = child.clone();
        assert!(std::thread::spawn(move || { let _held = poison.2.lock().unwrap(); panic!("retention poison fixture"); }).join().is_err());
        assert_eq!(child.try_retained_work_pending(4), Err(JournalRetentionError::Unavailable));
        assert!(child.has_unresolved_tasks());
    }
    #[test]
    fn retained_work_scan_keeps_report_completeness_and_actual_inputs_separate() {
        let root = SagaRollbackJournal::new("unfinished but no retained inputs");
        assert!(!root.snapshot().is_complete());
        assert_eq!(root.try_retained_work_pending(1), Ok(false));
        assert_eq!(root.try_retained_work_pending(0), Err(JournalRetentionError::TraversalLimit));
        let child = SagaRollbackJournal::new("original child input");
        root.retain_child(child.clone());
        child.push_uninvoked_task(ranvier_core::saga::SagaTask { node_id: "original".into(),
            node_label: "private".into(), input_snapshot: vec![3, 7, 9] });
        assert_eq!(root.try_retained_work_pending(2), Ok(true));
        assert_eq!(child.1.lock().unwrap()[0].1.input_snapshot, [3, 7, 9]);
    }
    #[test]
    fn repeated_child_identity_and_large_fanout_fail_closed() {
        let root = SagaRollbackJournal::new("bounded child graph");
        let child = SagaRollbackJournal::new("shared child");
        root.retain_child(child.clone()); root.retain_child(child);
        assert_eq!(root.try_retained_work_pending(4), Err(JournalRetentionError::TraversalLimit));
        assert!(root.has_unresolved_tasks());
        let wide = SagaRollbackJournal::new("wide graph");
        for _ in 0..256 { wide.retain_child(SagaRollbackJournal::new("child")); }
        assert_eq!(wide.try_retained_work_pending(256), Err(JournalRetentionError::TraversalLimit));
        assert_eq!(wide.try_retained_work_pending(257), Ok(false));
    }
    #[cfg(feature = "streaming")]
    #[test]
    fn live_probe_does_not_certify_complete_cyclic_recovery_graph() {
        use crate::streaming_report_handle::StreamRecoveryProbeError as Error;
        let root = SagaRollbackJournal::new("cycle root");
        let child = SagaRollbackJournal::new("cycle child");
        root.retain_child(child.clone()); child.retain_child(root.clone());
        root.0.lock().unwrap().steps[0].outcome = SagaRollbackOutcome::Completed; root.finish();
        child.0.lock().unwrap().steps[0].outcome = SagaRollbackOutcome::Completed; child.finish();
        let result = root.try_recovery_pending(4);
        root.3.lock().unwrap().clear(); child.3.lock().unwrap().clear();
        assert_eq!(result, Err(Error::TraversalLimit));
    }
    #[cfg(feature = "streaming")]
    #[test]
    fn live_probe_budget_covers_report_steps_before_scanning_them() {
        let journal = SagaRollbackJournal::new("large live probe report");
        for _ in 0..5 { journal.push("private step".into(), SagaRollbackOutcome::Completed); }
        journal.finish();
        assert_eq!(journal.try_recovery_pending(4), Err(crate::streaming_report_handle::StreamRecoveryProbeError::TraversalLimit));
        assert_eq!(journal.try_recovery_pending(6), Ok(false));
    }

}
