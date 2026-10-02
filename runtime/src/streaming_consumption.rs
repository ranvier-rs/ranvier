//! Owned consumption observations, separate from prefix durability and cleanup.
//! A completed source is not proof of durable business completion. Dropping a
//! consumer does not await cleanup and produces no terminal observation.
use crate::StreamTimeoutKind;
use futures_core::Stream;
use futures_util::StreamExt;
use ranvier_core::{CancellationContext, CancellationToken, streaming::StreamTimeoutConfig};
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::time::{Instant, Sleep};

/// The observed reason item consumption stopped. Cancellation wins a same-poll
/// deadline or ready item; a reached deadline wins source readiness. Total wins
/// an exact tie with init/idle. These observations do not certify compensation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamConsumptionTerminal {
    Completed,
    Cancelled(CancellationContext),
    TimedOut(StreamTimeoutKind),
    /// The configured duration cannot be represented by the runtime clock.
    InvalidDeadline(StreamTimeoutKind),
}

/// Observed items, including partial values, and the independently observed end.
/// Items require neither Clone nor Serialize nor Sync.
pub struct StreamConsumptionReport<Item> {
    pub items: Vec<Item>,
    pub terminal: StreamConsumptionTerminal,
}

/// An owned stream whose EOF has an explicit terminal observation.
/// This low-level owner will be used by the report-aware Axon consumer cutover.
/// It borrows neither caller Bus nor Resources and does not run Saga cleanup.
pub struct ObservedStream<Item> {
    inner: Option<Pin<Box<dyn Stream<Item = Item> + Send>>>,
    config: StreamTimeoutConfig,
    started_at: Instant,
    last_item_at: Option<Instant>,
    timer: Option<Pin<Box<Sleep>>>,
    cancellation: Option<Pin<Box<dyn Future<Output = CancellationContext> + Send>>>,
    terminal: Option<StreamConsumptionTerminal>,
}
impl<Item> ObservedStream<Item> {
    pub fn new(
        stream: Pin<Box<dyn Stream<Item = Item> + Send>>,
        config: StreamTimeoutConfig,
        token: Option<CancellationToken>,
    ) -> Self {
        Self {
            inner: Some(stream),
            config,
            started_at: Instant::now(),
            last_item_at: None,
            timer: None,
            cancellation: token.map(|token| {
                Box::pin(async move { token.cancelled().await })
                    as Pin<Box<dyn Future<Output = CancellationContext> + Send>>
            }),
            terminal: None,
        }
    }
    pub fn terminal(&self) -> Option<&StreamConsumptionTerminal> {
        self.terminal.as_ref()
    }
    fn finish(&mut self, terminal: StreamConsumptionTerminal) -> Poll<Option<Item>> {
        self.terminal = Some(terminal);
        self.inner = None;
        self.timer = None;
        self.cancellation = None;
        Poll::Ready(None)
    }
    /// Retain partial items on cancellation/timeout instead of returning a
    /// truncated vector as though the source completed. Dropping this future
    /// still produces no report and performs no asynchronous recovery.
    pub async fn collect_report(mut self) -> StreamConsumptionReport<Item> {
        let mut items = Vec::new();
        while let Some(item) = self.next().await {
            items.push(item);
        }
        // All EOF branches set terminal; callers cannot replace the source.
        StreamConsumptionReport {
            items,
            terminal: self.terminal.take().expect("EOF records its terminal"),
        }
    }
}
impl<Item> Stream for ObservedStream<Item> {
    type Item = Item;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Item>> {
        let this = self.get_mut();
        if this.terminal.is_some() {
            return Poll::Ready(None);
        }
        if let Some(cancellation) = &mut this.cancellation {
            if let Poll::Ready(context) = cancellation.as_mut().poll(cx) {
                return this.finish(StreamConsumptionTerminal::Cancelled(context));
            }
        }
        let active_phase = match this.last_item_at {
            Some(at) => this.config.idle.map(|d| (at, d, StreamTimeoutKind::Idle)),
            None => this
                .config
                .init
                .map(|d| (this.started_at, d, StreamTimeoutKind::Init)),
        };
        let active_total = this
            .config
            .total
            .map(|d| (this.started_at, d, StreamTimeoutKind::Total));
        for (origin, duration, kind) in active_total.into_iter().chain(active_phase) {
            if origin.checked_add(duration).is_none() {
                return this.finish(StreamConsumptionTerminal::InvalidDeadline(kind));
            }
        }
        let total = this
            .config
            .total
            .and_then(|d| this.started_at.checked_add(d))
            .map(|at| (at, StreamTimeoutKind::Total));
        let phase = match this.last_item_at {
            Some(at) => this
                .config
                .idle
                .and_then(|d| at.checked_add(d))
                .map(|at| (at, StreamTimeoutKind::Idle)),
            None => this
                .config
                .init
                .and_then(|d| this.started_at.checked_add(d))
                .map(|at| (at, StreamTimeoutKind::Init)),
        };
        let deadline = match (total, phase) {
            (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
            (a, b) => a.or(b),
        };
        if let Some((at, kind)) = deadline {
            if at <= Instant::now() {
                return this.finish(StreamConsumptionTerminal::TimedOut(kind));
            }
            let timer = this
                .timer
                .get_or_insert_with(|| Box::pin(tokio::time::sleep_until(at)));
            timer.as_mut().reset(at);
            if timer.as_mut().poll(cx).is_ready() {
                return this.finish(StreamConsumptionTerminal::TimedOut(kind));
            }
        }
        match this
            .inner
            .as_mut()
            .expect("unfinished owner has a source")
            .as_mut()
            .poll_next(cx)
        {
            Poll::Ready(Some(item)) => {
                this.last_item_at = Some(Instant::now());
                Poll::Ready(Some(item))
            }
            Poll::Ready(None) => this.finish(StreamConsumptionTerminal::Completed),
            Poll::Pending => Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;
    use ranvier_core::CancellationReason;
    use std::{cell::Cell, time::Duration};
    fn config() -> StreamTimeoutConfig {
        StreamTimeoutConfig {
            init: None,
            idle: None,
            total: None,
        }
    }
    #[tokio::test]
    async fn invalid_deadline_is_not_silently_disabled() {
        let report = ObservedStream::new(
            Box::pin(stream::repeat(1)),
            StreamTimeoutConfig {
                total: Some(Duration::MAX),
                ..config()
            },
            None,
        )
        .collect_report()
        .await;
        assert!(report.items.is_empty());
        assert_eq!(
            report.terminal,
            StreamConsumptionTerminal::InvalidDeadline(StreamTimeoutKind::Total)
        );
    }

    #[tokio::test]
    async fn terminal_releases_source_and_never_polls_it_again() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        struct Source(Arc<AtomicUsize>);
        impl Stream for Source {
            type Item = usize;
            fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<usize>> {
                Poll::Ready(None)
            }
        }
        impl Drop for Source {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let drops = Arc::new(AtomicUsize::new(0));
        let mut owner = ObservedStream::new(Box::pin(Source(drops.clone())), config(), None);
        assert_eq!(owner.next().await, None);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert_eq!(owner.next().await, None);
        assert_eq!(
            owner.terminal(),
            Some(&StreamConsumptionTerminal::Completed)
        );
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn collection_retains_move_only_non_sync_items_and_normal_terminal() {
        let report =
            ObservedStream::new(Box::pin(stream::iter(vec![Cell::new(7)])), config(), None)
                .collect_report()
                .await;
        assert_eq!(report.items[0].get(), 7);
        assert_eq!(report.terminal, StreamConsumptionTerminal::Completed);
    }
    #[tokio::test(start_paused = true)]
    async fn partial_items_keep_idle_timeout_distinct_from_completion() {
        let input = stream::iter(vec![Cell::new(9)]).chain(stream::pending());
        let report = ObservedStream::new(
            Box::pin(input),
            StreamTimeoutConfig {
                idle: Some(Duration::from_secs(1)),
                ..config()
            },
            None,
        )
        .collect_report()
        .await;
        assert_eq!(report.items[0].get(), 9);
        assert_eq!(
            report.terminal,
            StreamConsumptionTerminal::TimedOut(StreamTimeoutKind::Idle)
        );
    }
    #[tokio::test(start_paused = true)]
    async fn cancellation_wins_ready_item_and_zero_deadline() {
        let token = CancellationToken::new();
        token.cancel(CancellationReason::Explicit);
        let mut owner = ObservedStream::new(
            Box::pin(stream::repeat(1)),
            StreamTimeoutConfig {
                total: Some(Duration::ZERO),
                ..config()
            },
            Some(token),
        );
        assert_eq!(owner.next().await, None);
        assert!(
            matches!(owner.terminal(), Some(StreamConsumptionTerminal::Cancelled(context)) if context.reason == CancellationReason::Explicit)
        );
        assert_eq!(owner.next().await, None);
    }
    #[tokio::test(start_paused = true)]
    async fn pending_source_is_woken_and_total_wins_exact_tie() {
        let report = ObservedStream::new(
            Box::pin(stream::pending::<usize>()),
            StreamTimeoutConfig {
                init: Some(Duration::from_secs(1)),
                total: Some(Duration::from_secs(1)),
                ..config()
            },
            None,
        )
        .collect_report()
        .await;
        assert!(report.items.is_empty());
        assert_eq!(
            report.terminal,
            StreamConsumptionTerminal::TimedOut(StreamTimeoutKind::Total)
        );
    }
}
