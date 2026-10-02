//! Private task-frame custody. Production preparation pairs the gate and holder;
//! grant requires an exact live task registration. Child/future identity remains
//! a supervisor factory obligation, separate from holder registration proof.
//! No task output or panic payload is exposed through Debug or serialization.
use std::future::Future;
use tokio::{sync::oneshot, task::JoinHandle, time::Instant};

/// Own the exact start permit with its task; callers never select a permit.
pub(crate) struct PreparedStreamTask<T> {
    task: std::sync::Arc<tokio::sync::Mutex<ManagedStreamTask<T>>>,
    permit: Option<oneshot::Sender<()>>,
    grant_decided: bool,
    owner: Option<std::sync::Weak<dyn std::any::Any + Send + Sync>>,
}
impl<T: Send + 'static> PreparedStreamTask<T> {
    pub(crate) fn new<F: Future<Output = T> + Send + 'static>(future: F) -> Self {
        let (task, permit) = ManagedStreamTask::spawn_gated(future);
        Self { task: std::sync::Arc::new(tokio::sync::Mutex::new(task)), permit: Some(permit), grant_decided: false, owner: None }
    }
    pub(crate) fn new_owned<F: Future<Output = T> + Send + 'static>(owner: crate::nested_custody::Capability, future: F) -> Self {
        let mut prepared = Self::new(future);
        prepared.owner = Some(std::sync::Arc::downgrade(&owner));
        prepared
    }
    pub(crate) fn matches_owner(&self, owner: &crate::nested_custody::Capability) -> bool {
        self.owner.as_ref().is_some_and(|bound| std::sync::Weak::ptr_eq(bound, &std::sync::Arc::downgrade(owner)))
    }
    pub(crate) fn holder(&self) -> std::sync::Arc<tokio::sync::Mutex<ManagedStreamTask<T>>> {
        self.task.clone()
    }
    pub(crate) fn reject_start(&mut self) -> Result<(), ManagedTaskWaitError> {
        if self.grant_decided { return Err(ManagedTaskWaitError::StartGrantDecided); }
        self.permit.take();
        Ok(())
    }
    pub(crate) fn has_open_start(&self) -> bool { self.permit.is_some() && !self.grant_decided }
    #[cfg(test)]
    fn grant_start(&mut self) -> Result<(), &'static str> {
        let permit = self.permit.take().ok_or("execution.nested_task_start_decided")?;
        self.grant_decided = true;
        permit.send(()).map_err(|_| "execution.nested_task_start_unavailable")
    }
    pub(crate) fn grant_registered_start(&mut self, admission: &crate::nested_custody::WeakCustody) -> Result<(), &'static str> {
        let was_pending = self.permit.is_some();
        let task: crate::nested_custody::Capability = self.task.clone();
        let result = admission.grant_task_start(&task, &mut self.permit);
        if was_pending && self.permit.is_none() { self.grant_decided = true; }
        result
    }
    /// Reject synchronously, then borrow custody through lock/join deadline.
    pub(crate) fn reject_until(&mut self, deadline: Instant)
        -> impl Future<Output = Result<Result<Option<T>, tokio::task::JoinError>, ManagedTaskWaitError>> + '_ {
        if !self.grant_decided { self.permit.take(); }
        async move {
            if self.grant_decided { return Err(ManagedTaskWaitError::StartGrantDecided); }
            let mut task = tokio::time::timeout_at(deadline, self.task.lock()).await
                .map_err(|_| ManagedTaskWaitError::TimedOut)?;
            task.join_until(deadline).await
        }
    }
}

pub(crate) struct ManagedStreamTask<T> {
    handle: Option<JoinHandle<Option<T>>>,
    joined: Option<Result<Option<T>, tokio::task::JoinError>>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ManagedTaskWaitError { TimedOut, AlreadyJoined, StartGrantDecided }

impl<T: Send + 'static> ManagedStreamTask<T> {
    /// Reject the unopened start gate before returning the borrowed join wait.
    /// Even an unpolled/dropped wait cannot grant start; caller retains this
    /// task owner and must retry join after a timeout or dropped wait.
    #[cfg(test)]
    fn reject_start_until(
        &mut self, permit: oneshot::Sender<()>, deadline: Instant,
    ) -> impl Future<Output = Result<Result<Option<T>, tokio::task::JoinError>, ManagedTaskWaitError>> + '_ {
        drop(permit);
        self.join_until(deadline)
    }
    /// The future is never polled until the registering supervisor grants start.
    /// Dropping the permit rejects start; caller still owns and must join the task.
    fn spawn_gated<F>(future: F) -> (Self, oneshot::Sender<()>)
    where F: Future<Output = T> + Send + 'static {
        let (permit, gate) = oneshot::channel();
        let handle = tokio::spawn(async move {
            if gate.await.is_err() { return None; }
            Some(future.await)
        });
        (Self { handle: Some(handle), joined: None }, permit)
    }
    #[cfg(test)]
    pub(crate) fn prepare<F>(future: F) -> (Self, oneshot::Sender<()>)
    where F: Future<Output = T> + Send + 'static {
        Self::spawn_gated(future)
    }

    /// Borrow the handle rather than moving it into the deadline future.
    /// Timeout or dropped wait leaves task custody in this owner for later join.
    pub(crate) async fn join_until(
        &mut self, deadline: Instant,
    ) -> Result<Result<Option<T>, tokio::task::JoinError>, ManagedTaskWaitError> {
        self.settle_until(deadline).await?;
        self.take_joined().ok_or(ManagedTaskWaitError::AlreadyJoined)
    }

    /// Collect frame termination without moving its original typed report out.
    /// Completed results survive dropped family waits in this retained owner.
    pub(crate) async fn settle_until(&mut self, deadline: Instant) -> Result<(), ManagedTaskWaitError> {
        if self.joined.is_some() { return Ok(()); }
        let handle = self.handle.as_mut().ok_or(ManagedTaskWaitError::AlreadyJoined)?;
        match tokio::time::timeout_at(deadline, handle).await {
            Ok(result) => {
                self.handle.take();
                self.joined = Some(result);
                Ok(())
            },
            Err(_) => Err(ManagedTaskWaitError::TimedOut),
        }
    }

    pub(crate) fn take_joined(&mut self) -> Option<Result<Option<T>, tokio::task::JoinError>> {
        self.joined.take()
    }

}

impl<T> Drop for ManagedStreamTask<T> {
    fn drop(&mut self) {
        // Last-resort frame cancellation, not successful drain or effect reversal.
        // A managed root must retain this owner and await join before retirement.
        if let Some(handle) = &self.handle { handle.abort(); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
    #[tokio::test]
    async fn unavailable_start_preserves_cancelled_join_and_cannot_be_rejected_as_unstarted() {
        let effects = Arc::new(AtomicUsize::new(0));
        let observed = effects.clone();
        let mut prepared = PreparedStreamTask::new(async move { observed.fetch_add(1, Ordering::SeqCst); });
        let holder = prepared.holder();
        {
            let mut task = holder.lock().await;
            task.handle.as_ref().unwrap().abort();
            task.settle_until(Instant::now() + std::time::Duration::from_secs(5)).await.unwrap();
        }
        assert_eq!(prepared.grant_start(), Err("execution.nested_task_start_unavailable"));
        assert_eq!(prepared.grant_start(), Err("execution.nested_task_start_decided"));
        assert!(matches!(prepared.reject_until(Instant::now()).await, Err(ManagedTaskWaitError::StartGrantDecided)));
        assert!(holder.lock().await.take_joined().unwrap().unwrap_err().is_cancelled());
        assert_eq!(effects.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn reject_after_grant_does_not_join_or_consume_original_output() {
        let mut prepared = PreparedStreamTask::new(async { Box::new(113u32) });
        let holder = prepared.holder();
        prepared.grant_start().unwrap();
        assert!(matches!(prepared.reject_until(Instant::now()).await, Err(ManagedTaskWaitError::StartGrantDecided)));
        assert_eq!(*holder.lock().await.join_until(Instant::now() + std::time::Duration::from_secs(5)).await.unwrap().unwrap().unwrap(), 113);
    }
    #[tokio::test]
    async fn prepared_task_rejects_before_unpolled_wait_and_cannot_grant_later() {
        let effects = Arc::new(AtomicUsize::new(0));
        let observed = effects.clone();
        let mut prepared = PreparedStreamTask::new(async move { observed.fetch_add(1, Ordering::SeqCst); });
        drop(prepared.reject_until(Instant::now()));
        assert_eq!(prepared.grant_start(), Err("execution.nested_task_start_decided"));
        assert!(matches!(prepared.reject_until(Instant::now() + std::time::Duration::from_secs(5)).await, Ok(Ok(None))));
        assert_eq!(effects.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn prepared_task_grants_exact_holder_only_once() {
        let mut prepared = PreparedStreamTask::new(async { Box::new(109u32) });
        let holder = prepared.holder();
        assert!(Arc::ptr_eq(&holder, &prepared.holder()));
        prepared.grant_start().unwrap();
        assert_eq!(prepared.grant_start(), Err("execution.nested_task_start_decided"));
        drop(prepared);
        assert_eq!(*holder.lock().await.join_until(Instant::now() + std::time::Duration::from_secs(5)).await.unwrap().unwrap().unwrap(), 109);
    }
    #[tokio::test]
    async fn unpolled_rejection_wait_rejects_start_and_preserves_join_custody() {
        let effects = Arc::new(AtomicUsize::new(0));
        let observed = effects.clone();
        let (mut task, permit) = ManagedStreamTask::prepare(async move {
            observed.fetch_add(1, Ordering::SeqCst);
        });
        drop(task.reject_start_until(permit, Instant::now()));
        assert!(matches!(task.join_until(Instant::now() + std::time::Duration::from_secs(5)).await, Ok(Ok(None))));
        assert_eq!(effects.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn settled_report_is_retained_until_explicit_take() {
        let report = Arc::new(AtomicUsize::new(347));
        let weak = Arc::downgrade(&report);
        let (mut task, permit) = ManagedStreamTask::prepare(async move { report });
        permit.send(()).unwrap();
        task.settle_until(Instant::now() + std::time::Duration::from_secs(5)).await.unwrap();
        task.settle_until(Instant::now()).await.unwrap();
        assert!(weak.upgrade().is_some());
        let result = task.take_joined().unwrap().unwrap().unwrap();
        assert_eq!(result.load(Ordering::SeqCst), 347);
        assert!(task.take_joined().is_none());
        assert_eq!(task.settle_until(Instant::now()).await, Err(ManagedTaskWaitError::AlreadyJoined));
        drop(result);
        assert!(weak.upgrade().is_none());
    }

    #[tokio::test]
    async fn settled_panic_preserves_original_join_classification() {
        let (mut task, permit) = ManagedStreamTask::prepare(async { panic!("private fixture"); });
        permit.send(()).unwrap();
        task.settle_until(Instant::now() + std::time::Duration::from_secs(5)).await.unwrap();
        assert!(task.take_joined().unwrap().unwrap_err().is_panic());
    }

    #[tokio::test]
    async fn rejected_start_never_polls_effect_future_and_task_is_joined() {
        let effects = Arc::new(AtomicUsize::new(0));
        let observed = effects.clone();
        let (mut task, permit) = ManagedStreamTask::prepare(async move {
            observed.fetch_add(1, Ordering::SeqCst);
        });
        drop(permit);
        assert!(matches!(task.join_until(Instant::now() + std::time::Duration::from_secs(5)).await, Ok(Ok(None))));
        assert_eq!(effects.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn completed_join_cannot_be_polled_twice() {
        let (mut task, permit) = ManagedStreamTask::prepare(async { 319u32 });
        permit.send(()).unwrap();
        assert!(matches!(task.join_until(Instant::now() + std::time::Duration::from_secs(5)).await, Ok(Ok(Some(319)))));
        assert!(matches!(task.join_until(Instant::now()).await, Err(ManagedTaskWaitError::AlreadyJoined)));
    }

    #[tokio::test]
    async fn dropped_join_future_preserves_task_handle() {
        let (finish, gate) = oneshot::channel::<u32>();
        let (mut task, permit) = ManagedStreamTask::prepare(async move { gate.await.unwrap() });
        permit.send(()).unwrap();
        {
            let wait = task.join_until(Instant::now() + std::time::Duration::from_secs(5));
            tokio::pin!(wait);
            assert!(futures_util::poll!(&mut wait).is_pending());
        }
        finish.send(321).unwrap();
        assert!(matches!(task.join_until(Instant::now() + std::time::Duration::from_secs(5)).await, Ok(Ok(Some(321)))));
    }

    #[tokio::test(start_paused = true)]
    async fn timed_out_join_retains_actual_handle_for_retry() {
        let (finish, gate) = oneshot::channel::<u32>();
        let (mut task, permit) = ManagedStreamTask::prepare(async move { gate.await.unwrap() });
        permit.send(()).unwrap();
        assert!(task.join_until(Instant::now() + std::time::Duration::from_secs(3)).await.is_err());
        finish.send(317).unwrap();
        assert!(matches!(task.join_until(Instant::now() + std::time::Duration::from_secs(3)).await, Ok(Ok(Some(317)))));
    }
}
