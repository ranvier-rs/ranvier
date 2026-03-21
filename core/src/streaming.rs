//! # StreamingTransition: Incremental Data Production
//!
//! The `StreamingTransition` trait extends Ranvier's pipeline model to support
//! workloads that produce data incrementally — LLM token streaming, large file
//! processing, real-time event feeds.
//!
//! ## Design (DD-4 Option A)
//!
//! * **Separate trait** — does not modify existing `Transition` or `Outcome`
//! * **Terminal semantics** — `then_stream()` is the last step in an Axon chain
//! * **Bus snapshot** — Bus is available in `run_stream()` but not during streaming

use crate::bus::Bus;
use crate::transition::ResourceRequirement;
use async_trait::async_trait;
use futures_core::Stream;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::pin::Pin;
use std::time::Duration;

/// A streaming transition that produces a stream of items instead of a single Outcome.
///
/// This is the streaming counterpart to [`Transition`](crate::transition::Transition).
/// Where `Transition::run` returns a single `Outcome<To, Error>`,
/// `StreamingTransition::run_stream` returns a pinned `Stream` that yields items
/// incrementally.
///
/// ## Bus Access (Option 4A — Snapshot)
///
/// The `Bus` reference is available during `run_stream()` to read configuration,
/// authentication context, and other request-scoped data. However, the Bus is
/// **not** available during stream consumption — the stream runs independently
/// after `run_stream()` returns.
///
/// ## Terminal Semantics
///
/// A `StreamingTransition` is always the **last** step in an Axon chain.
/// You cannot chain `.then()` after `.then_stream()`.
///
/// ## Example
///
/// ```rust,ignore
/// use ranvier_core::streaming::StreamingTransition;
///
/// struct SynthesizeStream;
///
/// #[async_trait::async_trait]
/// impl StreamingTransition<ToolResults> for SynthesizeStream {
///     type Item = ChatChunk;
///     type Error = LlmError;
///     type Resources = AppResources;
///
///     async fn run_stream(
///         &self,
///         input: ToolResults,
///         resources: &AppResources,
///         bus: &mut Bus,
///     ) -> Result<Pin<Box<dyn Stream<Item = ChatChunk> + Send>>, LlmError> {
///         let stream = resources.llm.chat_stream(&input.prompt).await?;
///         Ok(Box::pin(stream))
///     }
/// }
/// ```
#[async_trait]
pub trait StreamingTransition<From>: Send + Sync + 'static
where
    From: Send + 'static,
{
    /// The type of each item yielded by the stream.
    type Item: Send + 'static;

    /// Domain-specific error type for stream initialization failures.
    type Error: Send + Sync + Debug + 'static;

    /// The type of resources required by this streaming transition.
    type Resources: ResourceRequirement;

    /// Produce a stream of items from the input.
    ///
    /// # Parameters
    ///
    /// * `input` — The data passed from the previous pipeline step
    /// * `resources` — Typed access to required resources
    /// * `bus` — Bus snapshot (read-only recommended; not available during streaming)
    ///
    /// # Returns
    ///
    /// On success, a pinned stream that yields `Self::Item` values.
    /// On failure, an error indicating why stream initialization failed.
    async fn run_stream(
        &self,
        input: From,
        resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Result<Pin<Box<dyn Stream<Item = Self::Item> + Send>>, Self::Error>;

    /// Returns a human-readable label for this streaming transition.
    /// Defaults to the type name.
    fn label(&self) -> String {
        let full = std::any::type_name::<Self>();
        full.split("::").last().unwrap_or(full).to_string()
    }

    /// Returns a description of what this streaming transition does.
    fn description(&self) -> Option<String> {
        None
    }
}

/// Blanket implementation for `Arc<T>` where `T: StreamingTransition`.
///
/// This allows sharing streaming transitions across multiple Axons.
#[async_trait]
impl<T, From> StreamingTransition<From> for std::sync::Arc<T>
where
    T: StreamingTransition<From> + Send + Sync + 'static,
    From: Send + 'static,
{
    type Item = T::Item;
    type Error = T::Error;
    type Resources = T::Resources;

    async fn run_stream(
        &self,
        input: From,
        resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Result<Pin<Box<dyn Stream<Item = Self::Item> + Send>>, Self::Error> {
        self.as_ref().run_stream(input, resources, bus).await
    }

    fn label(&self) -> String {
        self.as_ref().label()
    }

    fn description(&self) -> Option<String> {
        self.as_ref().description()
    }
}

// ---------------------------------------------------------------------------
// StreamEvent — SSE framing protocol
// ---------------------------------------------------------------------------

/// Protocol for SSE event framing.
///
/// Each item in a streaming pipeline is wrapped in a `StreamEvent` before
/// being sent over SSE. This follows the OpenAI SSE convention.
///
/// ## SSE Wire Format
///
/// ```text
/// StreamEvent::Data(T)  → data: {json}\n\n
/// StreamEvent::Error(e) → event: error\ndata: {json}\n\n
/// StreamEvent::Done     → data: [DONE]\n\n
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "payload")]
pub enum StreamEvent<T> {
    /// A data item in the stream.
    Data(T),
    /// A non-fatal error during streaming.
    Error(StreamError),
    /// End-of-stream signal.
    Done,
}

impl<T> StreamEvent<T> {
    /// Create a Data event.
    pub fn data(item: T) -> Self {
        Self::Data(item)
    }

    /// Create an Error event.
    pub fn error(message: impl Into<String>, code: impl Into<String>) -> Self {
        Self::Error(StreamError {
            message: message.into(),
            code: code.into(),
        })
    }

    /// Create a Done event.
    pub fn done() -> Self {
        Self::Done
    }

    /// Returns true if this is a Data event.
    pub fn is_data(&self) -> bool {
        matches!(self, Self::Data(_))
    }

    /// Returns true if this is an Error event.
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Error(_))
    }

    /// Returns true if this is a Done event.
    pub fn is_done(&self) -> bool {
        matches!(self, Self::Done)
    }
}

/// A non-fatal error that occurs during streaming.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StreamError {
    /// Human-readable error message.
    pub message: String,
    /// Machine-readable error code (e.g., "rate_limit", "timeout").
    pub code: String,
}

// ---------------------------------------------------------------------------
// StreamTimeoutConfig
// ---------------------------------------------------------------------------

/// Timeout configuration for streaming transitions.
///
/// Streaming has different timeout semantics than single-value transitions:
/// - **init**: Maximum time to produce the first item
/// - **idle**: Maximum time between consecutive items
/// - **total**: Maximum total stream duration
///
/// All fields are optional. `None` means no timeout for that phase.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StreamTimeoutConfig {
    /// Maximum time to produce the first item after stream initialization.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_duration_secs"
    )]
    pub init: Option<Duration>,

    /// Maximum time between consecutive items.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_duration_secs"
    )]
    pub idle: Option<Duration>,

    /// Maximum total stream duration.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_duration_secs"
    )]
    pub total: Option<Duration>,
}

impl StreamTimeoutConfig {
    /// Create a timeout config with all phases set.
    pub fn new(init: Duration, idle: Duration, total: Duration) -> Self {
        Self {
            init: Some(init),
            idle: Some(idle),
            total: Some(total),
        }
    }

    /// Create a config with only an idle timeout.
    pub fn idle_only(idle: Duration) -> Self {
        Self {
            init: None,
            idle: Some(idle),
            total: None,
        }
    }

    /// Create a config with no timeouts.
    pub fn none() -> Self {
        Self {
            init: None,
            idle: None,
            total: None,
        }
    }
}

impl Default for StreamTimeoutConfig {
    fn default() -> Self {
        Self::none()
    }
}

/// Serde helper for optional Duration as seconds (f64).
mod optional_duration_secs {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(val: &Option<Duration>, ser: S) -> Result<S::Ok, S::Error> {
        match val {
            Some(d) => d.as_secs_f64().serialize(ser),
            None => ser.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Option<Duration>, D::Error> {
        let opt: Option<f64> = Option::deserialize(de)?;
        Ok(opt.map(Duration::from_secs_f64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;

    // Mock streaming transition for testing
    struct MockStreamTransition;

    #[async_trait]
    impl StreamingTransition<String> for MockStreamTransition {
        type Item = String;
        type Error = String;
        type Resources = ();

        async fn run_stream(
            &self,
            input: String,
            _resources: &Self::Resources,
            _bus: &mut Bus,
        ) -> Result<Pin<Box<dyn Stream<Item = String> + Send>>, String> {
            let items = vec![
                format!("chunk1: {}", input),
                format!("chunk2: {}", input),
                format!("chunk3: {}", input),
            ];
            Ok(Box::pin(futures_util::stream::iter(items)))
        }
    }

    #[tokio::test]
    async fn test_streaming_transition_basic() {
        let transition = MockStreamTransition;
        let mut bus = Bus::new();
        let stream = transition
            .run_stream("hello".to_string(), &(), &mut bus)
            .await
            .unwrap();
        let items: Vec<String> = stream.collect().await;
        assert_eq!(items.len(), 3);
        assert_eq!(items[0], "chunk1: hello");
        assert_eq!(items[2], "chunk3: hello");
    }

    #[test]
    fn test_streaming_transition_label() {
        let t = MockStreamTransition;
        assert_eq!(t.label(), "MockStreamTransition");
    }

    #[tokio::test]
    async fn test_arc_streaming_transition() {
        let transition = std::sync::Arc::new(MockStreamTransition);
        let mut bus = Bus::new();
        let stream = transition
            .run_stream("arc".to_string(), &(), &mut bus)
            .await
            .unwrap();
        let items: Vec<String> = stream.collect().await;
        assert_eq!(items.len(), 3);
        assert_eq!(items[0], "chunk1: arc");
    }

    #[tokio::test]
    async fn test_streaming_transition_error() {
        struct FailingStream;

        #[async_trait]
        impl StreamingTransition<()> for FailingStream {
            type Item = String;
            type Error = String;
            type Resources = ();

            async fn run_stream(
                &self,
                _input: (),
                _resources: &(),
                _bus: &mut Bus,
            ) -> Result<Pin<Box<dyn Stream<Item = String> + Send>>, String> {
                Err("initialization failed".to_string())
            }
        }

        let transition = FailingStream;
        let mut bus = Bus::new();
        let result = transition.run_stream((), &(), &mut bus).await;
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert_eq!(err, "initialization failed");
    }

    // --- StreamEvent tests ---

    #[test]
    fn test_stream_event_data() {
        let event = StreamEvent::data("token");
        assert!(event.is_data());
        assert!(!event.is_error());
        assert!(!event.is_done());
    }

    #[test]
    fn test_stream_event_error() {
        let event: StreamEvent<String> = StreamEvent::error("rate limit", "rate_limit");
        assert!(event.is_error());
        match event {
            StreamEvent::Error(e) => {
                assert_eq!(e.message, "rate limit");
                assert_eq!(e.code, "rate_limit");
            }
            _ => panic!("Expected Error variant"),
        }
    }

    #[test]
    fn test_stream_event_done() {
        let event: StreamEvent<String> = StreamEvent::done();
        assert!(event.is_done());
    }

    #[test]
    fn test_stream_event_serialization_roundtrip() {
        let event = StreamEvent::data(42i32);
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: StreamEvent<i32> = serde_json::from_str(&json).unwrap();
        assert_eq!(event, deserialized);
    }

    #[test]
    fn test_stream_event_error_serialization() {
        let event: StreamEvent<String> = StreamEvent::error("timeout", "idle_timeout");
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("idle_timeout"));
        let deserialized: StreamEvent<String> = serde_json::from_str(&json).unwrap();
        assert_eq!(event, deserialized);
    }

    #[test]
    fn test_stream_event_done_serialization() {
        let event: StreamEvent<String> = StreamEvent::done();
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: StreamEvent<String> = serde_json::from_str(&json).unwrap();
        assert_eq!(event, deserialized);
    }

    // --- StreamTimeoutConfig tests ---

    #[test]
    fn test_stream_timeout_config_new() {
        let config = StreamTimeoutConfig::new(
            Duration::from_secs(5),
            Duration::from_secs(10),
            Duration::from_secs(60),
        );
        assert_eq!(config.init, Some(Duration::from_secs(5)));
        assert_eq!(config.idle, Some(Duration::from_secs(10)));
        assert_eq!(config.total, Some(Duration::from_secs(60)));
    }

    #[test]
    fn test_stream_timeout_config_idle_only() {
        let config = StreamTimeoutConfig::idle_only(Duration::from_secs(30));
        assert!(config.init.is_none());
        assert_eq!(config.idle, Some(Duration::from_secs(30)));
        assert!(config.total.is_none());
    }

    #[test]
    fn test_stream_timeout_config_none() {
        let config = StreamTimeoutConfig::none();
        assert!(config.init.is_none());
        assert!(config.idle.is_none());
        assert!(config.total.is_none());
    }

    #[test]
    fn test_stream_timeout_config_serialization() {
        let config = StreamTimeoutConfig::new(
            Duration::from_secs(5),
            Duration::from_millis(500),
            Duration::from_secs(120),
        );
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: StreamTimeoutConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, deserialized);
    }

    #[test]
    fn test_stream_timeout_config_none_serialization() {
        let config = StreamTimeoutConfig::none();
        let json = serde_json::to_string(&config).unwrap();
        // All fields should be omitted
        assert_eq!(json, "{}");
        let deserialized: StreamTimeoutConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, deserialized);
    }
}
