//! Private task-frame custody. Production preparation pairs the gate and holder;
//! grant requires an exact live task registration. Child/future identity remains
//! a supervisor factory obligation, separate from holder registration proof.
//! No task output or panic payload is exposed through Debug or serialization.
use std::future::Future;
use tokio::{sync::oneshot, task::JoinHandle, time::Instant};

/// Framework-only settlement closure retaining the exact original typed holder.
#[derive(Clone)]
pub(crate) struct TaskSettlement {
    holder: crate::nested_custody::Capability,
    settle: std::sync::Arc<
        dyn Fn(
                Instant,
            )
                -> std::pin::Pin<Box<dyn Future<Output = Result<(), ManagedTaskWaitError>> + Send>>
            + Send
            + Sync,
    >,
}
impl TaskSettlement {
    fn new<T: Send + 'static>(
        task: std::sync::Arc<tokio::sync::Mutex<ManagedStreamTask<T>>>,
    ) -> Self {
        let holder: crate::nested_custody::Capability = task.clone();
        Self {
            holder,
            settle: std::sync::Arc::new(move |deadline| {
                let task = task.clone();
                Box::pin(async move {
                    let mut task = tokio::time::timeout_at(deadline, task.lock())
                        .await
                        .map_err(|_| ManagedTaskWaitError::TimedOut)?;
                    task.settle_until(deadline).await
                })
            }),
        }
    }
    pub(crate) fn matches(&self, holder: &crate::nested_custody::Capability) -> bool {
        std::sync::Arc::ptr_eq(&self.holder, holder)
    }
    pub(crate) async fn settle_until(&self, deadline: Instant) -> Result<(), ManagedTaskWaitError> {
        (self.settle)(deadline).await
    }
}

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
        Self {
            task: std::sync::Arc::new(tokio::sync::Mutex::new(task)),
            permit: Some(permit),
            grant_decided: false,
            owner: None,
        }
    }
    pub(crate) fn new_owned<F: Future<Output = T> + Send + 'static>(
        owner: crate::nested_custody::Capability,
        future: F,
    ) -> Self {
        let mut prepared = Self::new(future);
        prepared.owner = Some(std::sync::Arc::downgrade(&owner));
        prepared
    }
    pub(crate) fn matches_owner(&self, owner: &crate::nested_custody::Capability) -> bool {
        self.owner
            .as_ref()
            .is_some_and(|bound| std::sync::Weak::ptr_eq(bound, &std::sync::Arc::downgrade(owner)))
    }
    pub(crate) fn holder(&self) -> std::sync::Arc<tokio::sync::Mutex<ManagedStreamTask<T>>> {
        self.task.clone()
    }
    pub(crate) fn settlement(&self) -> TaskSettlement {
        TaskSettlement::new(self.task.clone())
    }
    pub(crate) fn reject_start(&mut self) -> Result<(), ManagedTaskWaitError> {
        if self.grant_decided {
            return Err(ManagedTaskWaitError::StartGrantDecided);
        }
        self.permit.take();
        Ok(())
    }
    pub(crate) fn has_open_start(&self) -> bool {
        self.permit.is_some() && !self.grant_decided
    }
    pub(crate) fn grant_registered_start(
        &mut self,
        admission: &crate::nested_custody::WeakCustody,
    ) -> Result<(), &'static str> {
        let was_pending = self.permit.is_some();
        let task: crate::nested_custody::Capability = self.task.clone();
        let result = admission.grant_task_start(&task, &mut self.permit);
        if was_pending && self.permit.is_none() {
            self.grant_decided = true;
        }
        result
    }
}

pub(crate) struct ManagedStreamTask<T> {
    handle: Option<JoinHandle<Option<T>>>,
    joined: Option<Result<Option<T>, tokio::task::JoinError>>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ManagedTaskWaitError {
    TimedOut,
    AlreadyJoined,
    StartGrantDecided,
}

impl<T: Send + 'static> ManagedStreamTask<T> {
    /// The future is never polled until the registering supervisor grants start.
    /// Dropping the permit rejects start; caller still owns and must join the task.
    fn spawn_gated<F>(future: F) -> (Self, oneshot::Sender<()>)
    where
        F: Future<Output = T> + Send + 'static,
    {
        let (permit, gate) = oneshot::channel();
        let handle = tokio::spawn(async move {
            if gate.await.is_err() {
                return None;
            }
            Some(future.await)
        });
        (
            Self {
                handle: Some(handle),
                joined: None,
            },
            permit,
        )
    }

    /// Collect frame termination without moving its original typed report out.
    /// Completed results survive dropped family waits in this retained owner.
    pub(crate) async fn settle_until(
        &mut self,
        deadline: Instant,
    ) -> Result<(), ManagedTaskWaitError> {
        if self.joined.is_some() {
            return Ok(());
        }
        let handle = self
            .handle
            .as_mut()
            .ok_or(ManagedTaskWaitError::AlreadyJoined)?;
        match tokio::time::timeout_at(deadline, handle).await {
            Ok(result) => {
                self.handle.take();
                self.joined = Some(result);
                Ok(())
            }
            Err(_) => Err(ManagedTaskWaitError::TimedOut),
        }
    }

    /// Actual joined frame evidence only, without moving or formatting its report.
    pub(crate) fn joined_ref(&self) -> Option<&Result<Option<T>, tokio::task::JoinError>> {
        self.joined.as_ref()
    }
    pub(crate) fn take_joined(&mut self) -> Option<Result<Option<T>, tokio::task::JoinError>> {
        self.joined.take()
    }
}

impl<T> Drop for ManagedStreamTask<T> {
    fn drop(&mut self) {
        // Last-resort frame cancellation, not successful drain or effect reversal.
        // A managed root must retain this owner and await join before retirement.
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }
}
