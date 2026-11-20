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
    #[cfg_attr(
        not(feature = "streaming"),
        expect(
            dead_code,
            reason = "read only by streaming recovery until M427 operator recovery is wired"
        )
    )]
    pub(crate) context: crate::persistence::CompensationContext,
    #[cfg_attr(
        not(feature = "streaming"),
        expect(
            dead_code,
            reason = "read only by streaming recovery until M427 operator recovery is wired"
        )
    )]
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
    std::sync::Arc<
        std::sync::Mutex<
            Vec<(
                usize,
                tokio::sync::watch::Receiver<crate::streaming_recovery::SupervisorObservation>,
            )>,
        >,
    >,
    #[cfg(feature = "streaming")]
    std::sync::Arc<std::sync::Mutex<std::collections::HashMap<usize, std::sync::Arc<()>>>>,
);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JournalRetentionError {
    Unavailable,
    TraversalLimit,
}
#[cfg(feature = "streaming")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalTaskClaimError {
    Unavailable,
    Missing,
    NotEligible,
    Claimed,
    IdentityMismatch,
    TimedOut,
}
#[cfg(feature = "streaming")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExternalTaskClaimError {
    Unavailable,
    Missing,
    NotEligible,
    Claimed,
    IdentityMismatch,
    TimedOut,
}
#[cfg(feature = "streaming")]
struct PanicSafeDropFuture<F> {
    inner: Option<std::pin::Pin<Box<F>>>,
    drop_panicked: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
#[cfg(feature = "streaming")]
impl<F> Unpin for PanicSafeDropFuture<F> {}
#[cfg(feature = "streaming")]
impl<F> PanicSafeDropFuture<F> {
    fn new(future: F, drop_panicked: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Self {
        Self {
            inner: Some(Box::pin(future)),
            drop_panicked,
        }
    }
}
#[cfg(feature = "streaming")]
impl<F: std::future::Future> std::future::Future for PanicSafeDropFuture<F> {
    type Output = F::Output;
    fn poll(
        self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let Some(inner) = self.get_mut().inner.as_mut() else {
            return std::task::Poll::Pending;
        };
        inner.as_mut().poll(context)
    }
}
#[cfg(feature = "streaming")]
impl<F> Drop for PanicSafeDropFuture<F> {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take()
            && std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(inner))).is_err()
        {
            self.drop_panicked
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
}
/// Framework-only exclusive lease; the journal retains the original bytes.
/// This is not original owner, durable reconciliation or retirement authority.
#[cfg(feature = "streaming")]
pub(crate) struct LocalTaskClaim {
    journal: SagaRollbackJournal,
    index: usize,
    task: std::sync::Arc<ranvier_core::saga::SagaTask>,
    token: std::sync::Arc<()>,
    invoked: bool,
    settled: bool,
}
#[cfg(feature = "streaming")]
impl LocalTaskClaim {
    pub(crate) fn node_id(&self) -> &str {
        &self.task.node_id
    }
    pub(crate) fn mark_missing_handler(&mut self) -> Result<(), LocalTaskClaimError> {
        self.settle_original(SagaRollbackOutcome::MissingHandler)
    }
    /// Caller must bind this operation to the joined original typed owner and
    /// restore its Bus/control scope. No sync journal lock spans user code.
    pub(crate) async fn invoke<E, Res>(
        mut self,
        handler: ranvier_core::saga::SagaCompensationFn<E, Res>,
        resources: &Res,
        bus: &mut ranvier_core::Bus,
        deadline: tokio::time::Instant,
    ) -> Result<SagaRollbackOutcome, LocalTaskClaimError> {
        use futures_util::FutureExt as _;
        if tokio::time::Instant::now() >= deadline {
            return Err(LocalTaskClaimError::TimedOut);
        }
        self.validate_original()?;
        if tokio::time::Instant::now() >= deadline {
            return Err(LocalTaskClaimError::TimedOut);
        }
        self.invoked = true;
        let observed = tokio::time::timeout_at(
            deadline,
            std::panic::AssertUnwindSafe(async {
                // Classify and drop user result payloads inside the same panic
                // boundary as factory/poll. No raw E escapes into fixed metadata.
                match handler(&self.task.input_snapshot, resources, bus).await {
                    ranvier_core::Outcome::Next(()) => SagaRollbackOutcome::Completed,
                    ranvier_core::Outcome::Fault(_) => SagaRollbackOutcome::Fault,
                    ranvier_core::Outcome::Emit(_, _) => SagaRollbackOutcome::Emitted,
                    ranvier_core::Outcome::Branch(_, _) => SagaRollbackOutcome::Branched,
                    ranvier_core::Outcome::Jump(_, _) => SagaRollbackOutcome::Jumped,
                }
            })
            .catch_unwind(),
        )
        .await
        .map_err(|_| LocalTaskClaimError::TimedOut)?;
        let outcome = observed.unwrap_or(SagaRollbackOutcome::Panicked);
        self.settle_original(outcome)?;
        Ok(outcome)
    }
    fn validate_original(&self) -> Result<(), LocalTaskClaimError> {
        let report = self
            .journal
            .0
            .try_lock()
            .map_err(|_| LocalTaskClaimError::Unavailable)?;
        let claims = self
            .journal
            .6
            .try_lock()
            .map_err(|_| LocalTaskClaimError::Unavailable)?;
        let tasks = self
            .journal
            .1
            .try_lock()
            .map_err(|_| LocalTaskClaimError::Unavailable)?;
        if !claims
            .get(&self.index)
            .is_some_and(|token| std::sync::Arc::ptr_eq(token, &self.token))
            || !tasks.iter().any(|(index, task)| {
                *index == self.index && std::sync::Arc::ptr_eq(task, &self.task)
            })
        {
            return Err(LocalTaskClaimError::IdentityMismatch);
        }
        match report.steps.get(self.index) {
            Some(step)
                if step.node_id == self.task.node_id
                    && step.outcome == SagaRollbackOutcome::InProgress =>
            {
                Ok(())
            }
            _ => Err(LocalTaskClaimError::IdentityMismatch),
        }
    }
    fn settle_original(&mut self, outcome: SagaRollbackOutcome) -> Result<(), LocalTaskClaimError> {
        let mut report = self
            .journal
            .0
            .try_lock()
            .map_err(|_| LocalTaskClaimError::Unavailable)?;
        let mut claims = self
            .journal
            .6
            .try_lock()
            .map_err(|_| LocalTaskClaimError::Unavailable)?;
        let mut tasks = self
            .journal
            .1
            .try_lock()
            .map_err(|_| LocalTaskClaimError::Unavailable)?;
        let position = tasks
            .iter()
            .position(|(index, task)| {
                *index == self.index && std::sync::Arc::ptr_eq(task, &self.task)
            })
            .ok_or(LocalTaskClaimError::IdentityMismatch)?;
        if !claims
            .get(&self.index)
            .is_some_and(|token| std::sync::Arc::ptr_eq(token, &self.token))
        {
            return Err(LocalTaskClaimError::IdentityMismatch);
        }
        let step = report
            .steps
            .get_mut(self.index)
            .ok_or(LocalTaskClaimError::IdentityMismatch)?;
        if step.node_id != self.task.node_id || step.outcome != SagaRollbackOutcome::InProgress {
            return Err(LocalTaskClaimError::IdentityMismatch);
        }
        step.outcome = outcome;
        report.finished = false; // A single hook cannot seal the whole journal.
        if outcome == SagaRollbackOutcome::Completed {
            tasks.swap_remove(position);
            claims.remove(&self.index);
        }
        self.settled = true;
        Ok(())
    }
}
#[cfg(feature = "streaming")]
impl Drop for LocalTaskClaim {
    fn drop(&mut self) {
        if self.settled || self.invoked {
            return;
        }
        // Before invocation only, restore exact eligibility if all metadata is
        // available. Failure leaves InProgress, token and original bytes retained.
        let Ok(mut report) = self.journal.0.try_lock() else {
            return;
        };
        let Ok(mut claims) = self.journal.6.try_lock() else {
            return;
        };
        let Ok(tasks) = self.journal.1.try_lock() else {
            return;
        };
        if !claims
            .get(&self.index)
            .is_some_and(|token| std::sync::Arc::ptr_eq(token, &self.token))
            || !tasks.iter().any(|(index, task)| {
                *index == self.index && std::sync::Arc::ptr_eq(task, &self.task)
            })
        {
            return;
        }
        if let Some(step) = report.steps.get_mut(self.index) {
            if step.node_id == self.task.node_id && step.outcome == SagaRollbackOutcome::InProgress
            {
                step.outcome = SagaRollbackOutcome::NotInvoked;
                claims.remove(&self.index);
            }
        }
    }
}
#[cfg(feature = "streaming")]
pub(crate) struct ExternalTaskClaim {
    journal: SagaRollbackJournal,
    index: usize,
    recovery: ExternalCompensationRecovery,
    token: std::sync::Arc<()>,
    settled: bool,
}
#[cfg(feature = "streaming")]
impl ExternalTaskClaim {
    /// Read only the existing completion marker. A negative or failed read is
    /// not evidence that the external hook did not run and never retries it.
    pub(crate) async fn readback_until(
        mut self,
        deadline: tokio::time::Instant,
    ) -> Result<crate::streaming_report_handle::StreamExternalReadback, ExternalTaskClaimError>
    {
        use futures_util::FutureExt as _;
        if tokio::time::Instant::now() >= deadline {
            return Err(ExternalTaskClaimError::TimedOut);
        }
        self.validate_original()?;
        if tokio::time::Instant::now() >= deadline {
            return Err(ExternalTaskClaimError::TimedOut);
        }
        let Some(idempotency) = self.recovery.idempotency.clone() else {
            return Ok(crate::streaming_report_handle::StreamExternalReadback::ReadUnavailable);
        };
        let store = idempotency.store();
        let key = crate::persistence::compensation_idempotency_key(&self.recovery.context);
        let drop_panicked = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let readback = PanicSafeDropFuture::new(
            async move { store.was_compensated(&key).await },
            drop_panicked.clone(),
        );
        let checked = std::panic::AssertUnwindSafe(tokio::time::timeout_at(deadline, readback))
            .catch_unwind()
            .await;
        match checked {
            Err(_) => Ok(crate::streaming_report_handle::StreamExternalReadback::ReadUnavailable),
            Ok(Err(_)) => Err(ExternalTaskClaimError::TimedOut),
            Ok(Ok(_)) if tokio::time::Instant::now() >= deadline => {
                Err(ExternalTaskClaimError::TimedOut)
            }
            Ok(Ok(Err(_))) => {
                Ok(crate::streaming_report_handle::StreamExternalReadback::ReadUnavailable)
            }
            Ok(Ok(Ok(_))) if drop_panicked.load(std::sync::atomic::Ordering::SeqCst) => {
                Ok(crate::streaming_report_handle::StreamExternalReadback::ReadUnavailable)
            }
            Ok(Ok(Ok(false))) => {
                Ok(crate::streaming_report_handle::StreamExternalReadback::MarkerAbsent)
            }
            Ok(Ok(Ok(true))) => {
                self.settle_confirmed()?;
                Ok(crate::streaming_report_handle::StreamExternalReadback::MarkerPresent)
            }
        }
    }

    fn validate_original(&self) -> Result<(), ExternalTaskClaimError> {
        let report = self
            .journal
            .0
            .try_lock()
            .map_err(|_| ExternalTaskClaimError::Unavailable)?;
        let claims = self
            .journal
            .6
            .try_lock()
            .map_err(|_| ExternalTaskClaimError::Unavailable)?;
        let external = self
            .journal
            .2
            .try_lock()
            .map_err(|_| ExternalTaskClaimError::Unavailable)?;
        if !claims
            .get(&self.index)
            .is_some_and(|token| std::sync::Arc::ptr_eq(token, &self.token))
            || !external.iter().any(|(index, recovery)| {
                *index == self.index
                    && std::sync::Arc::ptr_eq(&recovery.invoked, &self.recovery.invoked)
            })
        {
            return Err(ExternalTaskClaimError::IdentityMismatch);
        }
        match report.steps.get(self.index) {
            Some(step)
                if step.node_id == "external-compensation"
                    && step.outcome != SagaRollbackOutcome::Completed =>
            {
                Ok(())
            }
            _ => Err(ExternalTaskClaimError::IdentityMismatch),
        }
    }

    fn settle_confirmed(&mut self) -> Result<(), ExternalTaskClaimError> {
        let released = {
            let mut report = self
                .journal
                .0
                .try_lock()
                .map_err(|_| ExternalTaskClaimError::Unavailable)?;
            let mut claims = self
                .journal
                .6
                .try_lock()
                .map_err(|_| ExternalTaskClaimError::Unavailable)?;
            let mut external = self
                .journal
                .2
                .try_lock()
                .map_err(|_| ExternalTaskClaimError::Unavailable)?;
            let position = external
                .iter()
                .position(|(index, recovery)| {
                    *index == self.index
                        && std::sync::Arc::ptr_eq(&recovery.invoked, &self.recovery.invoked)
                })
                .ok_or(ExternalTaskClaimError::IdentityMismatch)?;
            if !claims
                .get(&self.index)
                .is_some_and(|token| std::sync::Arc::ptr_eq(token, &self.token))
            {
                return Err(ExternalTaskClaimError::IdentityMismatch);
            }
            let step = report
                .steps
                .get_mut(self.index)
                .ok_or(ExternalTaskClaimError::IdentityMismatch)?;
            if step.node_id != "external-compensation"
                || step.outcome == SagaRollbackOutcome::Completed
            {
                return Err(ExternalTaskClaimError::IdentityMismatch);
            }
            step.outcome = SagaRollbackOutcome::Completed;
            // A single positive readback must not seal the whole journal or
            // authorize root retirement; later child/local work may remain.
            report.finished = false;
            claims.remove(&self.index);
            self.settled = true;
            external.swap_remove(position)
        };
        // Dropping user-provided hook/store objects may run destructors.
        drop(released);
        Ok(())
    }
}
#[cfg(feature = "streaming")]
impl Drop for ExternalTaskClaim {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let Ok(mut claims) = self.journal.6.try_lock() else {
            return;
        };
        if claims
            .get(&self.index)
            .is_some_and(|token| std::sync::Arc::ptr_eq(token, &self.token))
        {
            claims.remove(&self.index);
        }
    }
}
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
            #[cfg(feature = "streaming")]
            std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        )
    }
    #[cfg(feature = "streaming")]
    #[cfg(test)]
    pub(crate) fn claim_not_invoked(
        &self,
        index: usize,
    ) -> Result<LocalTaskClaim, LocalTaskClaimError> {
        let mut report = self
            .0
            .try_lock()
            .map_err(|_| LocalTaskClaimError::Unavailable)?;
        let mut claims = self
            .6
            .try_lock()
            .map_err(|_| LocalTaskClaimError::Unavailable)?;
        let tasks = self
            .1
            .try_lock()
            .map_err(|_| LocalTaskClaimError::Unavailable)?;
        if claims.contains_key(&index) {
            return Err(LocalTaskClaimError::Claimed);
        }
        let task = tasks
            .iter()
            .find(|(observed, _)| *observed == index)
            .map(|(_, task)| task.clone())
            .ok_or(LocalTaskClaimError::Missing)?;
        let step = report
            .steps
            .get_mut(index)
            .ok_or(LocalTaskClaimError::IdentityMismatch)?;
        if step.node_id != task.node_id {
            return Err(LocalTaskClaimError::IdentityMismatch);
        }
        if step.outcome != SagaRollbackOutcome::NotInvoked {
            return Err(LocalTaskClaimError::NotEligible);
        }
        let token = std::sync::Arc::new(());
        claims.insert(index, token.clone());
        step.outcome = SagaRollbackOutcome::InProgress;
        report.finished = false;
        Ok(LocalTaskClaim {
            journal: self.clone(),
            index,
            task,
            token,
            invoked: false,
            settled: false,
        })
    }
    /// Claim the first unresolved journal row. Earlier child/external/uncertain
    /// work blocks local compensation so recovery cannot silently reorder effects.
    #[cfg(feature = "streaming")]
    pub(crate) fn claim_next_not_invoked(&self) -> Result<LocalTaskClaim, LocalTaskClaimError> {
        let mut report = self
            .0
            .try_lock()
            .map_err(|_| LocalTaskClaimError::Unavailable)?;
        let mut claims = self
            .6
            .try_lock()
            .map_err(|_| LocalTaskClaimError::Unavailable)?;
        let tasks = self
            .1
            .try_lock()
            .map_err(|_| LocalTaskClaimError::Unavailable)?;
        let index = report
            .steps
            .iter()
            .position(|step| step.outcome != SagaRollbackOutcome::Completed)
            .ok_or(LocalTaskClaimError::Missing)?;
        if claims.contains_key(&index) {
            return Err(LocalTaskClaimError::Claimed);
        }
        let task = tasks
            .iter()
            .find(|(observed, _)| *observed == index)
            .map(|(_, task)| task.clone())
            .ok_or(LocalTaskClaimError::Missing)?;
        let step = report
            .steps
            .get_mut(index)
            .ok_or(LocalTaskClaimError::IdentityMismatch)?;
        if step.node_id != task.node_id {
            return Err(LocalTaskClaimError::IdentityMismatch);
        }
        if step.outcome != SagaRollbackOutcome::NotInvoked {
            return Err(LocalTaskClaimError::NotEligible);
        }
        let token = std::sync::Arc::new(());
        claims.insert(index, token.clone());
        step.outcome = SagaRollbackOutcome::InProgress;
        report.finished = false;
        Ok(LocalTaskClaim {
            journal: self.clone(),
            index,
            task,
            token,
            invoked: false,
            settled: false,
        })
    }
    /// Claim only the earliest unresolved journal row, and only when that row
    /// owns the exact retained external capability. Later rows cannot pass an
    /// earlier local/child/unknown outcome during readback.
    #[cfg(feature = "streaming")]
    pub(crate) fn claim_next_external(&self) -> Result<ExternalTaskClaim, ExternalTaskClaimError> {
        let report = self
            .0
            .try_lock()
            .map_err(|_| ExternalTaskClaimError::Unavailable)?;
        let mut claims = self
            .6
            .try_lock()
            .map_err(|_| ExternalTaskClaimError::Unavailable)?;
        let external = self
            .2
            .try_lock()
            .map_err(|_| ExternalTaskClaimError::Unavailable)?;
        let index = report
            .steps
            .iter()
            .position(|step| step.outcome != SagaRollbackOutcome::Completed)
            .ok_or(ExternalTaskClaimError::Missing)?;
        if claims.contains_key(&index) {
            return Err(ExternalTaskClaimError::Claimed);
        }
        let recovery = external
            .iter()
            .find(|(observed, _)| *observed == index)
            .map(|(_, recovery)| recovery.clone())
            .ok_or(ExternalTaskClaimError::NotEligible)?;
        let step = report
            .steps
            .get(index)
            .ok_or(ExternalTaskClaimError::IdentityMismatch)?;
        if step.node_id != "external-compensation" || step.outcome == SagaRollbackOutcome::Completed
        {
            return Err(ExternalTaskClaimError::NotEligible);
        }
        let token = std::sync::Arc::new(());
        claims.insert(index, token.clone());
        Ok(ExternalTaskClaim {
            journal: self.clone(),
            index,
            recovery,
            token,
            settled: false,
        })
    }
    /// Move a stack task into its original indexed journal without copying the
    /// task or its serialized input. On lock failure the exact task is returned.
    #[cfg(feature = "streaming")]
    pub(crate) fn try_push_uninvoked_task(
        &self,
        task: ranvier_core::saga::SagaTask,
    ) -> Result<usize, (LocalTaskClaimError, ranvier_core::saga::SagaTask)> {
        let mut report = match self.0.try_lock() {
            Ok(guard) => guard,
            Err(_) => return Err((LocalTaskClaimError::Unavailable, task)),
        };
        let _claims = match self.6.try_lock() {
            Ok(guard) => guard,
            Err(_) => return Err((LocalTaskClaimError::Unavailable, task)),
        };
        let mut tasks = match self.1.try_lock() {
            Ok(guard) => guard,
            Err(_) => return Err((LocalTaskClaimError::Unavailable, task)),
        };
        let index = report.steps.len();
        report.finished = false;
        report.steps.push(SagaRollbackStep {
            node_id: task.node_id.clone(),
            outcome: SagaRollbackOutcome::NotInvoked,
        });
        tasks.push((index, std::sync::Arc::new(task)));
        Ok(index)
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
        self.5
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push((index, observer));
    }
    #[cfg(feature = "streaming")]
    #[cfg(test)]
    pub(crate) fn retained_stream_observations(
        &self,
    ) -> Vec<(usize, crate::streaming_recovery::SupervisorObservation)> {
        self.5
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .map(|(index, observer)| (*index, observer.borrow().clone()))
            .collect()
    }
    /// Wait for the current child observation set using one absolute deadline.
    /// This does not cancel children or retire their recovery records. Returned
    /// and Interrupted are local execution termination, not durable completion.
    #[cfg(feature = "streaming")]
    #[cfg(test)]
    pub(crate) async fn drain_stream_children(
        &self,
        deadline: tokio::time::Instant,
    ) -> Result<(), usize> {
        use futures_util::{StreamExt, stream::FuturesUnordered};
        let observers: Vec<_> = self
            .5
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .map(|(_, observer)| observer.clone())
            .collect();
        let mut pending = FuturesUnordered::new();
        for mut observer in observers {
            pending.push(async move {
                loop {
                    let settled = {
                        let state = observer.borrow_and_update();
                        matches!(
                            state.phase,
                            crate::streaming_recovery::SupervisorPhase::Returned
                                | crate::streaming_recovery::SupervisorPhase::Interrupted
                        ) || state.custody != crate::streaming_recovery::RecoveryCustody::Held
                    };
                    if settled {
                        return true;
                    }
                    if observer.changed().await.is_err() {
                        return false;
                    }
                }
            });
        }
        let mut unknown = 0;
        while !pending.is_empty() {
            match tokio::time::timeout_at(deadline, pending.next()).await {
                Ok(Some(true)) => {}
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
            if retained.is_none() {
                *retained = incoming.take();
            }
        }
        // Any user resource destructor must run outside the journal lock.
        if let Some(incoming) = incoming {
            incoming.release();
        }
    }
    pub(crate) fn release_custody(&self) {
        let released = self.4.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(released) = released {
            released.release();
        }
    }
    #[cfg(all(test, feature = "streaming"))]
    pub(crate) fn retained_custody(
        &self,
    ) -> Option<std::sync::Arc<dyn std::any::Any + Send + Sync>> {
        self.4
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .and_then(|record| record.upgrade())
    }
    #[cfg(all(test, feature = "streaming"))]
    pub(crate) fn poison_retention_for_test(&self) {
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _held = self.1.lock().unwrap();
                panic!("retention poison fixture");
            }))
            .is_err()
        );
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
    pub(crate) fn try_recovery_pending(
        &self,
        limit: usize,
    ) -> Result<bool, crate::streaming_report_handle::StreamRecoveryProbeError> {
        use crate::streaming_report_handle::StreamRecoveryProbeError as Error;
        let mut pending = vec![self.clone()];
        let mut seen = std::collections::HashSet::new();
        let mut work = 0usize;
        while let Some(journal) = pending.pop() {
            if !seen.insert(std::sync::Arc::as_ptr(&journal.0) as usize) {
                return Err(Error::TraversalLimit);
            }
            let report = journal.0.try_lock().map_err(|_| Error::Unavailable)?;
            work = work.saturating_add(1).saturating_add(report.steps.len());
            if work > limit {
                return Err(Error::TraversalLimit);
            }
            let incomplete = !report.is_complete();
            drop(report);
            if incomplete
                || !journal
                    .1
                    .try_lock()
                    .map_err(|_| Error::Unavailable)?
                    .is_empty()
                || !journal
                    .2
                    .try_lock()
                    .map_err(|_| Error::Unavailable)?
                    .is_empty()
                || !journal
                    .5
                    .try_lock()
                    .map_err(|_| Error::Unavailable)?
                    .is_empty()
            {
                return Ok(true);
            }
            let children = journal.3.try_lock().map_err(|_| Error::Unavailable)?;
            if pending.len().saturating_add(children.len()) > limit {
                return Err(Error::TraversalLimit);
            }
            pending.extend(children.iter().map(|(_, child)| child.clone()));
        }
        Ok(false)
    }
    /// Presence of retained work, not hook completion or retirement permission.
    /// Nonblocking and bounded even during scope restoration or final owner Drop.
    /// Unknown state must retain custody rather than certify an empty journal.
    pub(crate) fn try_retained_work_pending(
        &self,
        limit: usize,
    ) -> Result<bool, JournalRetentionError> {
        let mut pending = vec![self.clone()];
        let mut seen = std::collections::HashSet::new();
        while let Some(journal) = pending.pop() {
            if seen.len() >= limit || !seen.insert(std::sync::Arc::as_ptr(&journal.0) as usize) {
                return Err(JournalRetentionError::TraversalLimit);
            }
            if !journal
                .1
                .try_lock()
                .map_err(|_| JournalRetentionError::Unavailable)?
                .is_empty()
                || !journal
                    .2
                    .try_lock()
                    .map_err(|_| JournalRetentionError::Unavailable)?
                    .is_empty()
            {
                return Ok(true);
            }
            #[cfg(feature = "streaming")]
            if !journal
                .5
                .try_lock()
                .map_err(|_| JournalRetentionError::Unavailable)?
                .is_empty()
            {
                return Ok(true);
            }
            let children = journal
                .3
                .try_lock()
                .map_err(|_| JournalRetentionError::Unavailable)?;
            if seen
                .len()
                .saturating_add(pending.len())
                .saturating_add(children.len())
                > limit
            {
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
    #[cfg(all(test, feature = "streaming"))]
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
    fn push_task_observed(
        &self,
        task: std::sync::Arc<ranvier_core::saga::SagaTask>,
        outcome: SagaRollbackOutcome,
    ) {
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
    #[cfg(test)]
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
            #[cfg(feature = "streaming")]
            let _claims = match self.6.try_lock() {
                Ok(claims) if !claims.contains_key(&index) => claims,
                _ => return,
            };
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
    #[cfg(test)]
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
        #[cfg(feature = "streaming")]
        let _claims = match self.6.try_lock() {
            Ok(claims) if !claims.contains_key(&report.steps.len().saturating_sub(1)) => claims,
            _ => return,
        };
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
    #[cfg(feature = "streaming")]
    fn claim_fixture() -> super::SagaRollbackJournal {
        let journal = super::SagaRollbackJournal::new("claim original");
        journal.push_uninvoked_task(ranvier_core::saga::SagaTask {
            node_id: "original".into(),
            node_label: "private".into(),
            input_snapshot: vec![4, 2, 7],
        });
        journal
    }
    #[cfg(feature = "streaming")]
    #[tokio::test]
    async fn stale_claim_token_cannot_invoke_original_hook() {
        let journal = claim_fixture();
        let claim = journal.claim_not_invoked(0).unwrap();
        journal.6.lock().unwrap().insert(0, std::sync::Arc::new(()));
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = calls.clone();
        let handler: ranvier_core::saga::SagaCompensationFn<String, ()> =
            std::sync::Arc::new(move |_, _, _| {
                observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async { ranvier_core::Outcome::Next(()) })
            });
        let result = claim
            .invoke(
                handler,
                &(),
                &mut ranvier_core::Bus::new(),
                tokio::time::Instant::now() + std::time::Duration::from_secs(5),
            )
            .await;
        assert_eq!(result, Err(super::LocalTaskClaimError::IdentityMismatch));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(journal.has_unresolved_tasks());
    }
    #[cfg(feature = "streaming")]
    #[tokio::test]
    async fn claim_classifies_panicking_returned_error_drop_without_losing_original_input() {
        struct PanickingError;
        impl Drop for PanickingError {
            fn drop(&mut self) {
                panic!("returned error drop fixture");
            }
        }
        let journal = claim_fixture();
        let claim = journal.claim_not_invoked(0).unwrap();
        let handler: ranvier_core::saga::SagaCompensationFn<PanickingError, ()> =
            std::sync::Arc::new(|_, _, _| {
                Box::pin(async { ranvier_core::Outcome::Fault(PanickingError) })
            });
        assert_eq!(
            claim
                .invoke(
                    handler,
                    &(),
                    &mut ranvier_core::Bus::new(),
                    tokio::time::Instant::now() + std::time::Duration::from_secs(5)
                )
                .await,
            Ok(super::SagaRollbackOutcome::Panicked)
        );
        assert_eq!(journal.1.lock().unwrap()[0].1.input_snapshot, [4, 2, 7]);
        assert!(matches!(
            journal.claim_not_invoked(0),
            Err(super::LocalTaskClaimError::Claimed)
        ));
    }
    #[cfg(feature = "streaming")]
    #[tokio::test]
    async fn unavailable_and_expired_claims_have_zero_hook_calls_and_retain_original_input() {
        use super::{LocalTaskClaimError as Error, SagaRollbackOutcome as Outcome};
        for expired in [false, true] {
            let journal = claim_fixture();
            let claim = journal.claim_not_invoked(0).unwrap();
            let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let observed = calls.clone();
            let handler: ranvier_core::saga::SagaCompensationFn<String, ()> =
                std::sync::Arc::new(move |_, _, _| {
                    observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Box::pin(async { ranvier_core::Outcome::Next(()) })
                });
            let held = (!expired).then(|| journal.6.lock().unwrap());
            let deadline = tokio::time::Instant::now()
                + if expired {
                    std::time::Duration::ZERO
                } else {
                    std::time::Duration::from_secs(5)
                };
            assert_eq!(
                claim
                    .invoke(handler, &(), &mut ranvier_core::Bus::new(), deadline)
                    .await,
                Err(if expired {
                    Error::TimedOut
                } else {
                    Error::Unavailable
                })
            );
            drop(held);
            assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
            assert_eq!(journal.1.lock().unwrap()[0].1.input_snapshot, [4, 2, 7]);
            assert_eq!(
                journal.snapshot().steps[0].outcome,
                if expired {
                    Outcome::NotInvoked
                } else {
                    Outcome::InProgress
                }
            );
        }
    }
    #[cfg(feature = "streaming")]
    #[test]
    fn claim_is_exclusive_and_uninvoked_drop_restores_original_entry() {
        use super::{LocalTaskClaimError as Error, SagaRollbackOutcome as Outcome};
        let journal = claim_fixture();
        let address = journal.1.lock().unwrap()[0].1.input_snapshot.as_ptr() as usize;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let (send, receive) = std::sync::mpsc::channel();
        let mut workers = Vec::new();
        for _ in 0..2 {
            let journal = journal.clone();
            let barrier = barrier.clone();
            let send = send.clone();
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                send.send(journal.claim_not_invoked(0)).unwrap();
            }));
        }
        barrier.wait();
        let results = [receive.recv().unwrap(), receive.recv().unwrap()];
        for worker in workers {
            worker.join().unwrap();
        }
        let mut winners = Vec::new();
        for result in results {
            match result {
                Ok(claim) => winners.push(claim),
                Err(error) => assert!(matches!(error, Error::Claimed | Error::Unavailable)),
            }
        }
        assert_eq!(winners.len(), 1);
        assert!(journal.has_unresolved_tasks());
        journal.settle(Outcome::Completed);
        journal.settle_external(0, Outcome::Completed);
        assert_eq!(journal.snapshot().steps[0].outcome, Outcome::InProgress);
        assert_eq!(
            journal.1.lock().unwrap()[0].1.input_snapshot.as_ptr() as usize,
            address
        );
        drop(winners);
        assert_eq!(journal.snapshot().steps[0].outcome, Outcome::NotInvoked);
        assert!(journal.claim_not_invoked(0).is_ok());
    }
    #[cfg(feature = "streaming")]
    #[tokio::test(start_paused = true)]
    async fn claim_timeout_and_pending_future_drop_hold_original_bytes_without_retry_permission() {
        use super::{LocalTaskClaimError as Error, SagaRollbackOutcome as Outcome};
        use futures_util::FutureExt as _;
        for timeout in [false, true] {
            let journal = claim_fixture();
            let address = journal.1.lock().unwrap()[0].1.input_snapshot.as_ptr() as usize;
            let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let observed = calls.clone();
            let handler: ranvier_core::saga::SagaCompensationFn<String, ()> =
                std::sync::Arc::new(move |input, _, _| {
                    assert_eq!(input.as_ptr() as usize, address);
                    observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Box::pin(std::future::pending())
                });
            let claim = journal.claim_not_invoked(0).unwrap();
            let mut bus = ranvier_core::Bus::new();
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
            if timeout {
                assert_eq!(
                    claim.invoke(handler, &(), &mut bus, deadline).await,
                    Err(Error::TimedOut)
                );
            } else {
                let future = claim.invoke(handler, &(), &mut bus, deadline);
                tokio::pin!(future);
                assert!(future.as_mut().now_or_never().is_none());
            }
            assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
            assert!(matches!(journal.claim_not_invoked(0), Err(Error::Claimed)));
            journal.settle(Outcome::Completed);
            assert_eq!(journal.snapshot().steps[0].outcome, Outcome::InProgress);
            assert_eq!(
                journal.1.lock().unwrap()[0].1.input_snapshot.as_ptr() as usize,
                address
            );
        }
    }
    #[cfg(feature = "streaming")]
    #[tokio::test]
    async fn claim_settles_exact_original_row_and_preserves_failed_hook_inputs() {
        use super::SagaRollbackOutcome as Outcome;
        for fail in [false, true] {
            let journal = claim_fixture();
            let claim = journal.claim_not_invoked(0).unwrap();
            journal.push("later independent row".into(), Outcome::InProgress);
            let handler: ranvier_core::saga::SagaCompensationFn<String, ()> =
                std::sync::Arc::new(move |_, _, _| {
                    Box::pin(async move {
                        if fail {
                            panic!("claim hook panic fixture");
                        }
                        ranvier_core::Outcome::Next(())
                    })
                });
            let result = claim
                .invoke(
                    handler,
                    &(),
                    &mut ranvier_core::Bus::new(),
                    tokio::time::Instant::now() + std::time::Duration::from_secs(5),
                )
                .await
                .unwrap();
            assert_eq!(
                result,
                if fail {
                    Outcome::Panicked
                } else {
                    Outcome::Completed
                }
            );
            let report = journal.snapshot();
            assert_eq!(report.steps[0].outcome, result);
            assert_eq!(report.steps[1].outcome, Outcome::InProgress);
            assert!(!report.finished);
            assert_eq!(journal.1.lock().unwrap().len(), usize::from(fail));
        }
    }
    #[tokio::test]
    async fn borrowed_hook_drop_panic_and_completion_preserve_original_allocation() {
        use futures_util::FutureExt as _;
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        for mode in 0..3 {
            let journal = super::SagaRollbackJournal::new("borrowed original input");
            let bytes = vec![3, 8, 1, 9];
            let address = bytes.as_ptr() as usize;
            let task = Arc::new(ranvier_core::saga::SagaTask {
                node_id: "original node".into(),
                node_label: "private label".into(),
                input_snapshot: bytes,
            });
            let weak = Arc::downgrade(&task);
            journal.push_task_shared(task.clone());
            let seen = Arc::new(AtomicUsize::new(0));
            let observed = seen.clone();
            let handler: ranvier_core::saga::SagaCompensationFn<String, ()> =
                Arc::new(move |input, _, _| {
                    let observed = observed.clone();
                    Box::pin(async move {
                        observed.store(input.as_ptr() as usize, Ordering::SeqCst);
                        assert_eq!(input, [3, 8, 1, 9]);
                        if mode == 1 {
                            panic!("borrowed hook panic fixture");
                        }
                        if mode == 2 {
                            return ranvier_core::Outcome::Next(());
                        }
                        std::future::pending::<ranvier_core::Outcome<(), String>>().await
                    })
                });
            let mut bus = ranvier_core::Bus::new();
            {
                let hook =
                    std::panic::AssertUnwindSafe(handler(&task.input_snapshot, &(), &mut bus))
                        .catch_unwind();
                tokio::pin!(hook);
                let result = hook.as_mut().now_or_never();
                match mode {
                    0 => assert!(result.is_none()),
                    1 => assert!(matches!(result, Some(Err(_)))),
                    _ => assert!(matches!(result, Some(Ok(ranvier_core::Outcome::Next(()))))),
                }
            }
            assert_eq!(seen.load(Ordering::SeqCst), address);
            if mode == 1 {
                journal.settle(super::SagaRollbackOutcome::Panicked);
            }
            if mode == 2 {
                journal.settle(super::SagaRollbackOutcome::Completed);
            }
            drop(task);
            let retained = journal.1.lock().unwrap();
            if mode == 2 {
                assert!(retained.is_empty());
                assert!(weak.upgrade().is_none());
            } else {
                assert_eq!(retained[0].1.input_snapshot.as_ptr() as usize, address);
                assert_eq!(Arc::strong_count(&retained[0].1), 1);
            }
            drop(retained);
            drop(journal);
            assert!(weak.upgrade().is_none());
        }
    }
    #[test]
    fn uninvoked_task_retention_moves_bytes_and_preserves_prior_observation() {
        let journal = super::SagaRollbackJournal::new("move-input");
        journal.push(
            "already-entered".into(),
            super::SagaRollbackOutcome::InProgress,
        );
        let bytes = vec![1, 2, 3, 4];
        let allocation = bytes.as_ptr();
        journal.push_uninvoked_task(ranvier_core::saga::SagaTask {
            node_id: "remaining".into(),
            node_label: "remaining".into(),
            input_snapshot: bytes,
        });
        let retained = journal.1.lock().unwrap();
        assert_eq!(retained[0].1.input_snapshot.as_ptr(), allocation);
        assert_eq!(retained[0].0, 1);
        drop(retained);
        let report = journal.snapshot();
        assert!(!report.finished);
        assert_eq!(
            report.steps[0].outcome,
            super::SagaRollbackOutcome::InProgress
        );
        assert_eq!(
            report.steps[1].outcome,
            super::SagaRollbackOutcome::NotInvoked
        );
    }
    use super::*;
    #[test]
    fn completed_external_settlement_releases_retained_recovery() {
        let journal = SagaRollbackJournal::new("release-probe");
        let context = crate::persistence::CompensationContext {
            trace_id: "release-probe".into(),
            circuit: "private-circuit".into(),
            fault_kind: "Fault".into(),
            fault_step: 1,
            timestamp_ms: 0,
            intent_sequence: Some(3),
        };
        let recovery = ExternalCompensationRecovery {
            context,
            idempotency: None,
            invoked: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        let index = journal.push_external(recovery);
        assert_eq!(journal.unresolved_external().len(), 1);
        journal.settle_external(index, SagaRollbackOutcome::Completed);
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
        let root = SagaRollbackJournal::new("private root trace");
        root.finish();
        assert_eq!(root.try_recovery_pending(4), Ok(false));
        let child = SagaRollbackJournal::new("private child trace");
        root.retain_child(child.clone());
        root.0.lock().unwrap().steps[0].outcome = SagaRollbackOutcome::Completed;
        root.finish();
        assert_eq!(root.try_recovery_pending(4), Ok(true));
        child.finish();
        assert_eq!(root.try_recovery_pending(1), Err(Error::TraversalLimit));
        assert_eq!(root.try_recovery_pending(4), Ok(false));
        let busy = child.1.lock().unwrap();
        assert_eq!(root.try_recovery_pending(4), Err(Error::Unavailable));
        drop(busy);
        let poisoned = child.clone();
        assert!(
            std::thread::spawn(move || {
                let _guard = poisoned.1.lock().unwrap();
                panic!("journal probe poison fixture");
            })
            .join()
            .is_err()
        );
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
        let worker = std::thread::spawn(move || {
            send.send(clone.has_unresolved_tasks()).unwrap();
        });
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
        assert_eq!(
            root.try_retained_work_pending(1),
            Err(JournalRetentionError::TraversalLimit)
        );
        child.retain_child(root.clone());
        let cyclic = root.try_retained_work_pending(4);
        let retained = root.has_unresolved_tasks();
        root.3.lock().unwrap().clear();
        child.3.lock().unwrap().clear();
        assert_eq!(cyclic, Err(JournalRetentionError::TraversalLimit));
        assert!(retained);
        let poison = child.clone();
        assert!(
            std::thread::spawn(move || {
                let _held = poison.2.lock().unwrap();
                panic!("retention poison fixture");
            })
            .join()
            .is_err()
        );
        assert_eq!(
            child.try_retained_work_pending(4),
            Err(JournalRetentionError::Unavailable)
        );
        assert!(child.has_unresolved_tasks());
    }
    #[test]
    fn retained_work_scan_keeps_report_completeness_and_actual_inputs_separate() {
        let root = SagaRollbackJournal::new("unfinished but no retained inputs");
        assert!(!root.snapshot().is_complete());
        assert_eq!(root.try_retained_work_pending(1), Ok(false));
        assert_eq!(
            root.try_retained_work_pending(0),
            Err(JournalRetentionError::TraversalLimit)
        );
        let child = SagaRollbackJournal::new("original child input");
        root.retain_child(child.clone());
        child.push_uninvoked_task(ranvier_core::saga::SagaTask {
            node_id: "original".into(),
            node_label: "private".into(),
            input_snapshot: vec![3, 7, 9],
        });
        assert_eq!(root.try_retained_work_pending(2), Ok(true));
        assert_eq!(child.1.lock().unwrap()[0].1.input_snapshot, [3, 7, 9]);
    }
    #[test]
    fn repeated_child_identity_and_large_fanout_fail_closed() {
        let root = SagaRollbackJournal::new("bounded child graph");
        let child = SagaRollbackJournal::new("shared child");
        root.retain_child(child.clone());
        root.retain_child(child);
        assert_eq!(
            root.try_retained_work_pending(4),
            Err(JournalRetentionError::TraversalLimit)
        );
        assert!(root.has_unresolved_tasks());
        let wide = SagaRollbackJournal::new("wide graph");
        for _ in 0..256 {
            wide.retain_child(SagaRollbackJournal::new("child"));
        }
        assert_eq!(
            wide.try_retained_work_pending(256),
            Err(JournalRetentionError::TraversalLimit)
        );
        assert_eq!(wide.try_retained_work_pending(257), Ok(false));
    }
    #[cfg(feature = "streaming")]
    #[test]
    fn live_probe_does_not_certify_complete_cyclic_recovery_graph() {
        use crate::streaming_report_handle::StreamRecoveryProbeError as Error;
        let root = SagaRollbackJournal::new("cycle root");
        let child = SagaRollbackJournal::new("cycle child");
        root.retain_child(child.clone());
        child.retain_child(root.clone());
        root.0.lock().unwrap().steps[0].outcome = SagaRollbackOutcome::Completed;
        root.finish();
        child.0.lock().unwrap().steps[0].outcome = SagaRollbackOutcome::Completed;
        child.finish();
        let result = root.try_recovery_pending(4);
        root.3.lock().unwrap().clear();
        child.3.lock().unwrap().clear();
        assert_eq!(result, Err(Error::TraversalLimit));
    }
    #[cfg(feature = "streaming")]
    #[test]
    fn live_probe_budget_covers_report_steps_before_scanning_them() {
        let journal = SagaRollbackJournal::new("large live probe report");
        for _ in 0..5 {
            journal.push("private step".into(), SagaRollbackOutcome::Completed);
        }
        journal.finish();
        assert_eq!(
            journal.try_recovery_pending(4),
            Err(crate::streaming_report_handle::StreamRecoveryProbeError::TraversalLimit)
        );
        assert_eq!(journal.try_recovery_pending(6), Ok(false));
    }
}
