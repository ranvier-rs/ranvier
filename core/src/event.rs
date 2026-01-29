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
