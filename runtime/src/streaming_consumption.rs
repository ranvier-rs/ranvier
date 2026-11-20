//! Owned consumption observations, separate from prefix durability and cleanup.
//! A completed source is not proof of durable business completion. Dropping a
//! consumer does not await cleanup and produces no terminal observation.
//!
//! Under a configured store the owner also records the stream effect's end
//! (Result, Terminal, completion) before yielding EOF; see [`StreamDurability`].
//! Dropping it before EOF records nothing and leaves the stream Intent open.
use crate::StreamTimeoutKind;
use crate::streaming_journal::{StreamDurability, StreamEnd, StreamJournal};
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
    /// Durability of the stream effect's end, separate from `terminal`.
    pub durability: StreamDurability,
}

type Recording = Pin<Box<dyn Future<Output = StreamDurability> + Send>>;

/// An owned stream whose EOF has an explicit terminal observation.
/// This low-level owner will be used by the report-aware Axon consumer cutover.
/// It borrows neither caller Bus nor Resources and does not run Saga cleanup.
pub struct ObservedStream<Item> {
    inner: Option<Pin<Box<dyn Stream<Item = Item> + Send>>>,
    config: StreamTimeoutConfig,
    started_at: Instant,
    total_started_at: Instant,
    last_item_at: Option<Instant>,
    timer: Option<Pin<Box<Sleep>>>,
    cancellation: Option<Pin<Box<dyn Future<Output = CancellationContext> + Send>>>,
    terminal: Option<StreamConsumptionTerminal>,
    journal: Option<StreamJournal>,
    recording: Option<Recording>,
    durability: Option<StreamDurability>,
    items_observed: u64,
    abandoned: bool,
}
impl<Item> ObservedStream<Item> {
    pub fn new(
        stream: Pin<Box<dyn Stream<Item = Item> + Send>>,
        config: StreamTimeoutConfig,
        token: Option<CancellationToken>,
    ) -> Self {
        Self::with_total_origin(stream, config, token, Instant::now())
    }
    /// Carry the invocation's original total-budget origin into consumption.
    /// First-item and idle deadlines retain their own consumption-phase clocks.
    /// This constructor does not enforce deadlines during prefix/initialization.
    pub(crate) fn with_total_origin(
        stream: Pin<Box<dyn Stream<Item = Item> + Send>>,
        config: StreamTimeoutConfig,
        token: Option<CancellationToken>,
        total_started_at: Instant,
    ) -> Self {
        Self {
            inner: Some(stream),
            config,
            started_at: Instant::now(),
            total_started_at,
            last_item_at: None,
            timer: None,
            cancellation: token.map(|token| {
                Box::pin(async move { token.cancelled().await })
                    as Pin<Box<dyn Future<Output = CancellationContext> + Send>>
            }),
            terminal: None,
            journal: None,
            recording: None,
            durability: None,
            items_observed: 0,
            abandoned: false,
        }
    }
    /// Attach the stream effect's journal; its Intent was acknowledged already.
    pub(crate) fn with_journal(mut self, journal: Option<StreamJournal>) -> Self {
        self.journal = journal;
        self
    }
    pub fn terminal(&self) -> Option<&StreamConsumptionTerminal> {
        self.terminal.as_ref()
    }
    /// Durability of the stream effect's end. `None` until EOF (or
    /// [`Self::abandon`]) has returned; the consumption terminal does not imply it.
    pub fn durability(&self) -> Option<&StreamDurability> {
        self.durability.as_ref()
    }
    /// Stop consuming before EOF and record the end as abandoned, for example
    /// after a client disconnect. Already delivered items are not reverted.
    /// Returns the existing durability when the end was already recorded. A
    /// failed write leaves the stream Intent open for reconciliation.
    pub async fn abandon(&mut self) -> StreamDurability {
        if self.terminal.is_some() || self.abandoned {
            futures_util::future::poll_fn(|cx| self.poll_record(cx)).await;
            return self
                .durability
                .clone()
                .expect("recording finishes with durability");
        }
        self.abandoned = true;
        self.inner = None;
        self.timer = None;
        self.cancellation = None;
        let items = self.items_observed;
        self.recording = Some(match self.journal.take() {
            None => Box::pin(async { StreamDurability::Unpersisted }),
            Some(journal) => {
                Box::pin(async move { journal.finish(StreamEnd::Abandoned, Some(items)).await })
            }
        });
        futures_util::future::poll_fn(|cx| self.poll_record(cx)).await;
        self.durability
            .clone()
            .expect("recording finishes with durability")
    }
    /// Record the observed terminal once, before EOF is yielded.
    fn poll_record(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        if self.durability.is_some() {
            return Poll::Ready(());
        }
        if self.recording.is_none() {
            let journal = self.journal.take();
            let end = self.terminal.as_ref().map(StreamEnd::from_terminal);
            let items = self.items_observed;
            self.recording = Some(match (journal, end) {
                (Some(journal), Some(end)) => {
                    Box::pin(async move { journal.finish(end, Some(items)).await })
                }
                _ => Box::pin(async { StreamDurability::Unpersisted }),
            });
        }
        let recording = self
            .recording
            .as_mut()
            .expect("recording was just installed");
        let durability = std::task::ready!(recording.as_mut().poll(cx));
        self.recording = None;
        self.durability = Some(durability);
        Poll::Ready(())
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
        // All EOF branches set terminal and durability; callers cannot replace the source.
        StreamConsumptionReport {
            items,
            terminal: self.terminal.take().expect("EOF records its terminal"),
            durability: self.durability.take().expect("EOF records its durability"),
        }
    }
}
impl<Item> ObservedStream<Item> {
    /// Observe control without polling another source item during backpressure.
    pub(crate) async fn stopped(&mut self) -> StreamConsumptionTerminal {
        futures_util::future::poll_fn(|cx| self.poll_stop(cx)).await;
        self.terminal
            .clone()
            .expect("ready control records terminal")
    }
    fn poll_stop(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        if self.terminal.is_some() {
            return Poll::Ready(());
        }
        if let Some(cancellation) = &mut self.cancellation {
            if let Poll::Ready(context) = cancellation.as_mut().poll(cx) {
                return self
                    .finish(StreamConsumptionTerminal::Cancelled(context))
                    .map(|_| ());
            }
        }
        let active_phase = match self.last_item_at {
            Some(at) => self.config.idle.map(|d| (at, d, StreamTimeoutKind::Idle)),
            None => self
                .config
                .init
                .map(|d| (self.started_at, d, StreamTimeoutKind::Init)),
        };
        let active_total = self
            .config
            .total
            .map(|d| (self.total_started_at, d, StreamTimeoutKind::Total));
        for (origin, duration, kind) in active_total.into_iter().chain(active_phase) {
            if origin.checked_add(duration).is_none() {
                return self
                    .finish(StreamConsumptionTerminal::InvalidDeadline(kind))
                    .map(|_| ());
            }
        }
        let total = self
            .config
            .total
            .and_then(|d| self.total_started_at.checked_add(d))
            .map(|at| (at, StreamTimeoutKind::Total));
        let phase = match self.last_item_at {
            Some(at) => self
                .config
                .idle
                .and_then(|d| at.checked_add(d))
                .map(|at| (at, StreamTimeoutKind::Idle)),
            None => self
                .config
                .init
                .and_then(|d| self.started_at.checked_add(d))
                .map(|at| (at, StreamTimeoutKind::Init)),
        };
        let deadline = match (total, phase) {
            (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
            (a, b) => a.or(b),
        };
        if let Some((at, kind)) = deadline {
            if at <= Instant::now() {
                return self
                    .finish(StreamConsumptionTerminal::TimedOut(kind))
                    .map(|_| ());
            }
            let timer = self
                .timer
                .get_or_insert_with(|| Box::pin(tokio::time::sleep_until(at)));
            timer.as_mut().reset(at);
            if timer.as_mut().poll(cx).is_ready() {
                return self
                    .finish(StreamConsumptionTerminal::TimedOut(kind))
                    .map(|_| ());
            }
        }
        Poll::Pending
    }
}
impl<Item> Stream for ObservedStream<Item> {
    type Item = Item;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Item>> {
        let this = self.get_mut();
        if this.abandoned {
            return this.poll_record(cx).map(|()| None);
        }
        if this.poll_stop(cx).is_pending() {
            match this
                .inner
                .as_mut()
                .expect("unfinished owner has a source")
                .as_mut()
                .poll_next(cx)
            {
                Poll::Ready(Some(item)) => {
                    this.last_item_at = Some(Instant::now());
                    this.items_observed = this.items_observed.saturating_add(1);
                    return Poll::Ready(Some(item));
                }
                Poll::Ready(None) => {
                    let _ = this.finish(StreamConsumptionTerminal::Completed);
                }
                Poll::Pending => return Poll::Pending,
            }
        }
        // The terminal is observed; EOF waits for the journal's acknowledgments.
        this.poll_record(cx).map(|()| None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;
    use ranvier_core::CancellationReason;

    #[tokio::test(start_paused = true)]
    async fn original_total_origin_expires_before_ready_source_is_polled() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let origin = Instant::now();
        tokio::time::advance(std::time::Duration::from_secs(8)).await;
        let polls = Arc::new(AtomicUsize::new(0));
        let count = polls.clone();
        let source = stream::poll_fn(move |_| {
            count.fetch_add(1, Ordering::SeqCst);
            Poll::Ready(Some(1))
        });
        let report = ObservedStream::with_total_origin(
            Box::pin(source),
            StreamTimeoutConfig {
                init: Some(std::time::Duration::from_secs(30)),
                idle: None,
                total: Some(std::time::Duration::from_secs(5)),
            },
            None,
            origin,
        )
        .collect_report()
        .await;
        assert!(report.items.is_empty());
        assert_eq!(
            report.terminal,
            StreamConsumptionTerminal::TimedOut(StreamTimeoutKind::Total)
        );
        assert_eq!(polls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn original_total_origin_does_not_age_first_item_or_idle_budget() {
        let origin = Instant::now();
        tokio::time::advance(std::time::Duration::from_secs(8)).await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let source = stream::poll_fn(move |cx| rx.poll_recv(cx));
        let mut observed = ObservedStream::with_total_origin(
            Box::pin(source),
            StreamTimeoutConfig {
                init: Some(std::time::Duration::from_secs(3)),
                idle: Some(std::time::Duration::from_secs(2)),
                total: Some(std::time::Duration::from_secs(20)),
            },
            None,
            origin,
        );
        tokio::time::advance(std::time::Duration::from_secs(2)).await;
        tx.send(1).unwrap();
        assert_eq!(observed.next().await, Some(1));
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        tx.send(2).unwrap();
        assert_eq!(observed.next().await, Some(2));
        tokio::time::advance(std::time::Duration::from_secs(2)).await;
        assert_eq!(observed.next().await, None);
        assert_eq!(
            observed.terminal(),
            Some(&StreamConsumptionTerminal::TimedOut(
                StreamTimeoutKind::Idle
            ))
        );
        drop(tx);
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_control_wins_expired_original_total_budget() {
        let origin = Instant::now();
        tokio::time::advance(std::time::Duration::from_secs(8)).await;
        let token = CancellationToken::new();
        token.cancel(CancellationReason::Explicit);
        let report = ObservedStream::with_total_origin(
            Box::pin(stream::repeat(1)),
            StreamTimeoutConfig {
                init: None,
                idle: None,
                total: Some(std::time::Duration::from_secs(5)),
            },
            Some(token),
            origin,
        )
        .collect_report()
        .await;
        assert!(report.items.is_empty());
        assert!(
            matches!(report.terminal, StreamConsumptionTerminal::Cancelled(context)
            if context.reason == CancellationReason::Explicit)
        );
    }

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
