use async_trait::async_trait;

/// Represents a source of events (e.g., WebSocket, Timer, Stream).
#[async_trait]
pub trait EventSource<E>: Send + Sync {
    /// Returns the next event, or None if the source is exhausted/closed.
    async fn next_event(&mut self) -> Option<E>;
}

/// Represents a sink for events (e.g., WebSocket, Log, Database).
#[async_trait]
pub trait EventSink<E>: Send + Sync {
    type Error: Send + Sync + 'static;

    /// Sends an event to the sink.
    async fn send_event(&self, event: E) -> Result<(), Self::Error>;
}

/// Defines the policy for handling failed events (Dead Letter Queue behavior).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum DlqPolicy {
    /// Drop the failed event.
    #[default]
    Drop,
    /// Send the event to a configured DLQ.
    SendToDlq,
    /// Retry a specific number of times with an exponential backoff before sending to DLQ.
    RetryThenDlq { max_attempts: u32, backoff_ms: u64 },
}

/// A Dead Letter Queue sink for storing failed events or workflow state.
#[async_trait]
pub trait DlqSink: Send + Sync {
    /// Save a failed event/payload with contextual information.
    async fn store_dead_letter(
        &self,
        workflow_id: &str,
        circuit_label: &str,
        node_id: &str,
        error_msg: &str,
        payload: &[u8],
    ) -> Result<(), String>;
}

/// A single dead letter entry retrieved from the DLQ.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DeadLetter {
    pub timestamp: String,
    pub workflow_id: String,
    pub circuit_label: String,
    pub node_id: String,
    pub error: String,
    pub payload_base64: String,
}

/// Read-side interface for querying the Dead Letter Queue.
#[async_trait]
pub trait DlqReader: Send + Sync {
    /// List dead letters, optionally filtered by workflow_id.
    async fn list_dead_letters(
        &self,
        workflow_filter: Option<&str>,
        limit: usize,
    ) -> Result<Vec<DeadLetter>, String>;

    /// Count dead letters in the queue.
    async fn count_dead_letters(&self) -> Result<u64, String>;
}
