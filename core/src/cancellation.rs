//! Cooperative cancellation control for one execution family.
//!
//! Cancellation is infrastructure control rather than domain
//! [`Outcome`](crate::outcome::Outcome)
//! control. Adapters create a root token, pass child tokens through execution,
//! and transitions may observe the token through [`Bus`](crate::bus::Bus).

use serde::{Deserialize, Serialize};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::watch;
use tokio::time::Instant;

/// The fixed, secret-free source of a cancellation request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancellationReason {
    /// A managed process or server is shutting down.
    OperatorShutdown,
    /// The execution's declared deadline elapsed.
    DeadlineExceeded,
    /// The protocol peer disconnected before execution completed.
    ClientDisconnected,
    /// An application or embedding runtime explicitly requested cancellation.
    Explicit,
}

/// Serializable cancellation metadata safe for persistence and diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancellationContext {
    /// Fixed cancellation source.
    pub reason: CancellationReason,
    /// Unix timestamp in milliseconds when cancellation was requested.
    pub requested_at_ms: u64,
    /// Optional absolute Unix deadline in milliseconds.
    pub deadline_ms: Option<u64>,
}

impl CancellationContext {
    /// Create an immediate cancellation context.
    pub fn new(reason: CancellationReason) -> Self {
        Self {
            reason,
            requested_at_ms: now_ms(),
            deadline_ms: None,
        }
    }

    fn deadline(deadline_ms: u64) -> Self {
        Self {
            reason: CancellationReason::DeadlineExceeded,
            requested_at_ms: deadline_ms,
            deadline_ms: Some(deadline_ms),
        }
    }
}

#[derive(Clone)]
struct CancellationRecord {
    order: u64,
    context: CancellationContext,
}

struct CancellationFamily {
    next_order: AtomicU64,
}

#[derive(Clone, Copy)]
struct CancellationDeadline {
    instant: Instant,
    unix_ms: u64,
}

struct CancellationInner {
    state: watch::Sender<Option<CancellationRecord>>,
    parent: Option<CancellationToken>,
    family: Arc<CancellationFamily>,
    deadline: Option<CancellationDeadline>,
}

/// Cloneable, first-wins cooperative cancellation token.
///
/// Child cancellation never cancels its parent. Parent cancellation is
/// observed by every descendant without a relay task or global registry.
#[derive(Clone)]
pub struct CancellationToken {
    inner: Arc<CancellationInner>,
}

impl std::fmt::Debug for CancellationToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancellationToken")
            .field("context", &self.context())
            .finish_non_exhaustive()
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancellationToken {
    /// Create a new uncancelled root token.
    pub fn new() -> Self {
        let family = Arc::new(CancellationFamily {
            next_order: AtomicU64::new(1),
        });
        Self::from_parts(None, family, None)
    }

    fn from_parts(
        parent: Option<CancellationToken>,
        family: Arc<CancellationFamily>,
        deadline: Option<CancellationDeadline>,
    ) -> Self {
        let (state, _receiver) = watch::channel(None);
        Self {
            inner: Arc::new(CancellationInner {
                state,
                parent,
                family,
                deadline,
            }),
        }
    }

    /// Create a child that observes this token but can be cancelled locally.
    pub fn child_token(&self) -> Self {
        Self::from_parts(Some(self.clone()), self.inner.family.clone(), None)
    }

    /// Create a child with a deadline relative to now.
    pub fn child_with_deadline(&self, timeout: Duration) -> Self {
        let now = Instant::now();
        let wall_now_ms = now_ms();
        let (instant, unix_ms) = match now.checked_add(timeout) {
            Some(deadline) => (deadline, wall_now_ms.saturating_add(duration_ms(timeout))),
            None => (now, wall_now_ms),
        };
        Self::from_parts(
            Some(self.clone()),
            self.inner.family.clone(),
            Some(CancellationDeadline { instant, unix_ms }),
        )
    }

    /// Request cancellation with a fixed reason.
    ///
    /// Returns `true` only when this token records a new local request. An
    /// earlier parent, deadline, or local request remains authoritative.
    pub fn cancel(&self, reason: CancellationReason) -> bool {
        self.cancel_with_context(CancellationContext::new(reason))
    }

    /// Request cancellation with an existing structured context.
    pub fn cancel_with_context(&self, context: CancellationContext) -> bool {
        self.settle_elapsed_deadline();
        if self.context_without_settle().is_some() {
            return false;
        }
        self.record(context)
    }

    /// Return whether this token or one of its parents has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.context().is_some()
    }

    /// Return the authoritative cancellation context, if any.
    pub fn context(&self) -> Option<CancellationContext> {
        self.settle_elapsed_deadline();
        self.context_without_settle().map(|record| record.context)
    }

    /// Wait until this token, an ancestor, or its deadline is cancelled.
    pub async fn cancelled(&self) -> CancellationContext {
        loop {
            if let Some(context) = self.context() {
                return context;
            }

            let mut receiver = self.inner.state.subscribe();
            let parent = self.inner.parent.clone();
            let deadline = self.closest_deadline();

            // `watch::subscribe` considers the current value seen. Recheck
            // after subscribing so a cancellation linearized between the
            // first check and subscription cannot become a lost wakeup.
            if let Some(context) = self.context() {
                return context;
            }

            let parent_wait = async move {
                match parent {
                    Some(parent) => Box::pin(parent.cancelled()).await,
                    None => std::future::pending::<CancellationContext>().await,
                }
            };
            let deadline_wait = async move {
                match deadline {
                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                    None => std::future::pending::<()>().await,
                }
            };

            tokio::select! {
                changed = receiver.changed() => {
                    if changed.is_err() {
                        std::future::pending::<()>().await;
                    }
                }
                _ = parent_wait => {}
                _ = deadline_wait => {
                    self.settle_elapsed_deadline();
                }
            }
        }
    }

    fn record(&self, context: CancellationContext) -> bool {
        let order = self.inner.family.next_order.fetch_add(1, Ordering::SeqCst);
        self.inner.state.send_if_modified(|state| {
            if state.is_none() {
                *state = Some(CancellationRecord { order, context });
                true
            } else {
                false
            }
        })
    }

    fn settle_elapsed_deadline(&self) {
        let now = Instant::now();
        let mut cursor = Some(self.clone());
        let mut elapsed: Option<(CancellationToken, CancellationDeadline)> = None;

        while let Some(token) = cursor {
            if let Some(deadline) = token.inner.deadline
                && deadline.instant <= now
                && elapsed
                    .as_ref()
                    .is_none_or(|(_, current)| deadline.instant < current.instant)
            {
                elapsed = Some((token.clone(), deadline));
            }
            cursor = token.inner.parent.clone();
        }

        if let Some((token, deadline)) = elapsed {
            token.record(CancellationContext::deadline(deadline.unix_ms));
        }
    }

    fn context_without_settle(&self) -> Option<CancellationRecord> {
        let mut cursor = Some(self.clone());
        let mut selected: Option<CancellationRecord> = None;

        while let Some(token) = cursor {
            if let Some(record) = token.inner.state.borrow().clone()
                && selected
                    .as_ref()
                    .is_none_or(|current| record.order < current.order)
            {
                selected = Some(record);
            }
            cursor = token.inner.parent.clone();
        }

        selected
    }

    fn closest_deadline(&self) -> Option<Instant> {
        let mut cursor = Some(self.clone());
        let mut selected: Option<Instant> = None;
        while let Some(token) = cursor {
            if let Some(deadline) = token.inner.deadline
                && selected.is_none_or(|current| deadline.instant < current)
            {
                selected = Some(deadline.instant);
            }
            cursor = token.inner.parent.clone();
        }
        selected
    }
}

fn now_ms() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_local_cancellation_wins_and_serializes_without_payloads() {
        let token = CancellationToken::new();
        assert!(token.cancel(CancellationReason::Explicit));
        assert!(!token.cancel(CancellationReason::OperatorShutdown));

        let context = token.context().expect("cancelled context");
        assert_eq!(context.reason, CancellationReason::Explicit);
        let json = serde_json::to_string(&context).expect("serialize context");
        assert!(json.contains("explicit"));
        assert!(!json.contains("authorization"));
        assert!(!json.contains("payload"));
    }

    #[test]
    fn parent_propagates_downward_but_child_does_not_cancel_parent() {
        let parent = CancellationToken::new();
        let child = parent.child_token();
        assert!(child.cancel(CancellationReason::ClientDisconnected));
        assert!(!parent.is_cancelled());

        let sibling = parent.child_token();
        assert!(parent.cancel(CancellationReason::OperatorShutdown));
        assert_eq!(
            sibling.context().map(|context| context.reason),
            Some(CancellationReason::OperatorShutdown)
        );
        assert_eq!(
            child.context().map(|context| context.reason),
            Some(CancellationReason::ClientDisconnected)
        );
    }

    #[tokio::test]
    async fn elapsed_deadline_precedes_later_explicit_request() {
        let root = CancellationToken::new();
        let child = root.child_with_deadline(Duration::from_millis(5));
        tokio::time::sleep(Duration::from_millis(15)).await;

        assert!(!child.cancel(CancellationReason::Explicit));
        let context = child.cancelled().await;
        assert_eq!(context.reason, CancellationReason::DeadlineExceeded);
        assert_eq!(context.deadline_ms, Some(context.requested_at_ms));
    }

    #[tokio::test]
    async fn awaiting_child_observes_parent_without_relay_task() {
        let parent = CancellationToken::new();
        let child = parent.child_token();
        let waiter = tokio::spawn(async move { child.cancelled().await });

        assert!(parent.cancel(CancellationReason::OperatorShutdown));
        let context = waiter.await.expect("join cancellation waiter");
        assert_eq!(context.reason, CancellationReason::OperatorShutdown);
    }

    #[test]
    fn dropping_child_does_not_cancel_parent() {
        let parent = CancellationToken::new();
        let child = parent.child_token();
        drop(child);
        assert!(!parent.is_cancelled());
    }
}
