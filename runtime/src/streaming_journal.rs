//! Durable journal of one configured-store stream effect (D-STREAM-DESIGN S1-S8).
//!
//! The whole stream is one effect: an Intent is acknowledged before the
//! initializer runs, and the Result, Terminal and optional completion are
//! recorded only after the consumption terminal is observed. Items are never
//! journaled and a stream is never replayed. A stream dropped without a
//! terminal leaves its Intent open, so a later load reports `PendingIntent`.
use crate::execution_report::StoreConfirmation;
use crate::{CompletionConfirmation, CompletionState, PersistenceHandle, PersistenceOperation};
use serde::{Deserialize, Serialize};

/// Durability of the stream effect's end, reported separately from the
/// consumption terminal. Neither certifies the other.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "durability", rename_all = "snake_case")]
pub enum StreamDurability {
    /// No execution-trace store was configured for this stream.
    Unpersisted,
    /// Result and Terminal were acknowledged. Completion may be app-owned.
    Recorded { completion: CompletionConfirmation },
    /// The named write was not acknowledged; it may still have committed.
    /// The trace requires reconciliation and is never replayed automatically.
    Unconfirmed { operation: PersistenceOperation },
}

/// How the stream effect ended, as written to the journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamEnd {
    Completed,
    TimedOut,
    InvalidDeadline,
    Cancelled,
    InitFailed,
    InitCancelled,
    Abandoned,
    DriverPanicked,
}

impl StreamEnd {
    pub(crate) fn from_terminal(terminal: &crate::StreamConsumptionTerminal) -> Self {
        match terminal {
            crate::StreamConsumptionTerminal::Completed => Self::Completed,
            crate::StreamConsumptionTerminal::Cancelled(_) => Self::Cancelled,
            crate::StreamConsumptionTerminal::TimedOut(_) => Self::TimedOut,
            crate::StreamConsumptionTerminal::InvalidDeadline(_) => Self::InvalidDeadline,
        }
    }
    fn result_kind(self) -> &'static str {
        match self {
            Self::Completed => "StreamCompleted",
            Self::TimedOut | Self::InvalidDeadline => "StreamTimedOut",
            Self::Cancelled => "StreamCancelled",
            Self::InitFailed => "StreamInitFailed",
            Self::InitCancelled => "StreamInitCancelled",
            Self::Abandoned => "StreamAbandoned",
            Self::DriverPanicked => "StreamDriverPanicked",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::TimedOut => "timed_out",
            Self::InvalidDeadline => "invalid_deadline",
            Self::Cancelled => "cancelled",
            Self::InitFailed => "init_failed",
            Self::InitCancelled => "init_cancelled",
            Self::Abandoned => "abandoned",
            Self::DriverPanicked => "driver_panicked",
        }
    }
    fn terminal_kind(self) -> &'static str {
        match self {
            Self::Completed => "Next",
            Self::TimedOut | Self::InvalidDeadline | Self::InitFailed | Self::DriverPanicked => {
                "Fault"
            }
            Self::Cancelled | Self::InitCancelled | Self::Abandoned => "Cancelled",
        }
    }
    fn completion(self) -> CompletionState {
        match self {
            Self::Completed => CompletionState::Success,
            Self::TimedOut | Self::InvalidDeadline | Self::InitFailed | Self::DriverPanicked => {
                CompletionState::Fault
            }
            Self::Cancelled | Self::InitCancelled | Self::Abandoned => CompletionState::Cancelled,
        }
    }
}

/// Initialized source and, under a configured store, the journal whose Intent
/// was acknowledged before the initializer ran. The consumer owns both.
pub(crate) struct InitializedStream<Item> {
    pub(crate) source: std::pin::Pin<Box<dyn futures_core::Stream<Item = Item> + Send>>,
    pub(crate) journal: Option<StreamJournal>,
}

impl<Item: Send + 'static> InitializedStream<Item> {
    pub(crate) fn map_source(
        self,
        map: impl FnOnce(
            std::pin::Pin<Box<dyn futures_core::Stream<Item = Item> + Send>>,
        ) -> std::pin::Pin<Box<dyn futures_core::Stream<Item = Item> + Send>>,
    ) -> Self {
        Self {
            source: map(self.source),
            journal: self.journal,
        }
    }
}

/// Owned writer for the stream node's events. It shares the invocation's
/// event-sequence allocator and outlives the invocation that created it.
#[derive(Clone)]
pub(crate) struct StreamJournal {
    handle: PersistenceHandle,
    trace_id: String,
    circuit: String,
    version: String,
    step: u64,
    node_id: String,
    auto_complete: bool,
}

impl std::fmt::Debug for StreamJournal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamJournal")
            .field("step", &self.step)
            .finish_non_exhaustive()
    }
}

impl StreamJournal {
    pub(crate) fn new(
        handle: PersistenceHandle,
        trace_id: String,
        circuit: String,
        version: String,
        step: u64,
        node_id: String,
        auto_complete: bool,
    ) -> Self {
        Self {
            handle,
            trace_id,
            circuit,
            version,
            step,
            node_id,
            auto_complete,
        }
    }

    pub(crate) fn step(&self) -> u64 {
        self.step
    }

    pub(crate) fn node_id(&self) -> &str {
        &self.node_id
    }

    /// Result of the stream node. `items` is `None` when the consumer did not count.
    pub(crate) async fn result(&self, end: StreamEnd, items: Option<u64>) -> StoreConfirmation {
        crate::axon::persist_execution_event(
            &self.handle,
            &self.trace_id,
            &self.circuit,
            &self.version,
            self.step,
            Some(self.node_id.clone()),
            end.result_kind(),
            Some(serde_json::json!({
                "terminal": end.label(),
                "items_observed": items,
            })),
        )
        .await
    }

    /// Terminal cursor after the stream node, then completion when enabled.
    pub(crate) async fn terminal(&self, end: StreamEnd) -> StreamDurability {
        let terminal = crate::axon::persist_execution_event(
            &self.handle,
            &self.trace_id,
            &self.circuit,
            &self.version,
            self.step.saturating_add(1),
            None,
            end.terminal_kind(),
            Some(serde_json::json!({ "stream": end.label() })),
        )
        .await;
        if terminal == StoreConfirmation::Unconfirmed {
            return StreamDurability::Unconfirmed {
                operation: PersistenceOperation::Terminal,
            };
        }
        if !self.auto_complete {
            return StreamDurability::Recorded {
                completion: CompletionConfirmation::NotRequested,
            };
        }
        match crate::axon::persist_completion(&self.handle, &self.trace_id, end.completion()).await
        {
            StoreConfirmation::Confirmed => StreamDurability::Recorded {
                completion: CompletionConfirmation::Confirmed,
            },
            StoreConfirmation::Unconfirmed => StreamDurability::Unconfirmed {
                operation: PersistenceOperation::Complete,
            },
        }
    }

    /// Result, then Terminal and completion. A lost Result stops every later write.
    pub(crate) async fn finish(&self, end: StreamEnd, items: Option<u64>) -> StreamDurability {
        if self.result(end, items).await == StoreConfirmation::Unconfirmed {
            return StreamDurability::Unconfirmed {
                operation: PersistenceOperation::Result,
            };
        }
        self.terminal(end).await
    }
}
