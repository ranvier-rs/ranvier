//! Private task-frame custody. Registration/root-family integration is separate.
//! No task output or panic payload is exposed through Debug or serialization.
use std::future::Future;
use tokio::{sync::oneshot, task::JoinHandle, time::Instant};

pub(crate) struct ManagedStreamTask<T> {
    handle: Option<JoinHandle<Option<T>>>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ManagedTaskWaitError { TimedOut, AlreadyJoined }

impl<T: Send + 'static> ManagedStreamTask<T> {
    /// The future is never polled until the registering supervisor grants start.
    /// Dropping the permit rejects start; caller still owns and must join the task.
    pub(crate) fn prepare<F>(future: F) -> (Self, oneshot::Sender<()>)
    where F: Future<Output = T> + Send + 'static {
        let (permit, gate) = oneshot::channel();
        let handle = tokio::spawn(async move {
            if gate.await.is_err() { return None; }
            Some(future.await)
        });
        (Self { handle: Some(handle) }, permit)
    }

    /// Borrow the handle rather than moving it into the deadline future.
    /// Timeout or dropped wait leaves task custody in this owner for later join.
    pub(crate) async fn join_until(
        &mut self, deadline: Instant,
    ) -> Result<Result<Option<T>, tokio::task::JoinError>, ManagedTaskWaitError> {
        let handle = self.handle.as_mut().ok_or(ManagedTaskWaitError::AlreadyJoined)?;
        match tokio::time::timeout_at(deadline, handle).await {
            Ok(result) => { self.handle.take(); Ok(result) },
            Err(_) => Err(ManagedTaskWaitError::TimedOut),
        }
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
