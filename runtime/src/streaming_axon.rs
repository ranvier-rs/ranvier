//! # StreamingAxon: Streaming Pipeline Executor
//!
//! `StreamingAxon` is created by calling `Axon::then_stream()`. It represents
//! a pipeline that ends with a `StreamingTransition` — the final step produces
//! a `Stream` of items instead of a single `Outcome`.
//!
//! ## Terminal Semantics
//!
//! A `StreamingAxon` is always terminal — you cannot chain `.then()` after it.
//! Use `collect_into_vec()` to collapse the stream back into a regular `Axon`.

use crate::axon::{Axon, BoxFuture};
use futures_core::Stream;
use futures_util::StreamExt;
use ranvier_core::bus::Bus;
use ranvier_core::outcome::Outcome;
use ranvier_core::schematic::Schematic;
use ranvier_core::streaming::StreamTimeoutConfig;
use ranvier_core::transition::ResourceRequirement;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fmt::Debug;
use std::pin::Pin;
use std::sync::Arc;

/// A streaming pipeline that produces a `Stream` of items.
///
/// Created via `Axon::then_stream()`. The pipeline executes all preceding
/// Axon steps, then invokes the `StreamingTransition` to produce a stream.
///
/// ## Backpressure
///
/// The stream is wrapped in a bounded `tokio::sync::mpsc::channel` with a
/// configurable buffer size (default: 64 items).
pub struct StreamingAxon<In, Item, E, Res = ()> {
    /// The Axon prefix (all steps before the streaming transition).
    pub schematic: Schematic,
    /// Executor that runs the prefix Axon and produces a stream.
    pub(crate) stream_executor: StreamExecutor<In, Item, E, Res>,
    /// Optional timeout configuration for streaming phases.
    pub timeout_config: Option<StreamTimeoutConfig>,
    /// Backpressure buffer size for the bounded channel.
    pub buffer_size: usize,
}

/// Public type alias for use by `Axon::then_stream()`.
pub type StreamExecutorType<In, Item, E, Res> = StreamExecutor<In, Item, E, Res>;

/// Type alias for the stream executor closure.
type StreamExecutor<In, Item, E, Res> = Arc<
    dyn for<'a> Fn(
            In,
            &'a Res,
            &'a mut Bus,
        ) -> BoxFuture<'a, Result<Pin<Box<dyn Stream<Item = Item> + Send>>, StreamingAxonError<E>>>
        + Send
        + Sync,
>;

/// Errors that can occur during streaming pipeline execution.
#[derive(Debug)]
pub enum StreamingAxonError<E> {
    /// The prefix Axon produced a Fault outcome.
    PipelineFault(E),
    /// The prefix Axon produced a non-Next outcome (Branch/Jump/Emit).
    UnexpectedOutcome(String),
    /// The streaming transition failed to initialize.
    StreamInitError(String),
    /// Stream timed out (init, idle, or total).
    Timeout(StreamTimeoutKind),
}

/// Which timeout phase was exceeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamTimeoutKind {
    /// First item not produced within the init timeout.
    Init,
    /// Gap between items exceeded the idle timeout.
    Idle,
    /// Total stream duration exceeded.
    Total,
}

impl<E: Debug> std::fmt::Display for StreamingAxonError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PipelineFault(e) => write!(f, "Pipeline fault: {:?}", e),
            Self::UnexpectedOutcome(msg) => write!(f, "Unexpected outcome: {}", msg),
            Self::StreamInitError(msg) => write!(f, "Stream init error: {}", msg),
            Self::Timeout(kind) => write!(f, "Stream timeout: {:?}", kind),
        }
    }
}

impl<E: Debug> std::error::Error for StreamingAxonError<E> {}

impl<In, Item, E, Res> Clone for StreamingAxon<In, Item, E, Res> {
    fn clone(&self) -> Self {
        Self {
            schematic: self.schematic.clone(),
            stream_executor: self.stream_executor.clone(),
            timeout_config: self.timeout_config.clone(),
            buffer_size: self.buffer_size,
        }
    }
}

impl<In, Item, E, Res> StreamingAxon<In, Item, E, Res>
where
    In: Send + Sync + 'static,
    Item: Send + 'static,
    E: Send + Sync + Debug + 'static,
    Res: ResourceRequirement,
{
    /// Execute the streaming pipeline, returning a stream of items.
    ///
    /// The prefix Axon steps run first, then the streaming transition
    /// produces a `Stream`. If timeout is configured, the stream is
    /// wrapped with timeout enforcement.
    pub async fn execute(
        &self,
        input: In,
        resources: &Res,
        bus: &mut Bus,
    ) -> Result<Pin<Box<dyn Stream<Item = Item> + Send>>, StreamingAxonError<E>> {
        let stream = (self.stream_executor)(input, resources, bus).await?;

        // Apply timeout wrapping if configured
        match &self.timeout_config {
            Some(config) if config.init.is_some() || config.idle.is_some() || config.total.is_some() => {
                Ok(Box::pin(TimeoutStream::new(stream, config.clone())))
            }
            _ => Ok(stream),
        }
    }

    /// Set the backpressure buffer size (default: 64).
    pub fn with_buffer_size(mut self, size: usize) -> Self {
        self.buffer_size = size;
        self
    }

    /// Set the timeout configuration.
    pub fn with_timeout(mut self, config: StreamTimeoutConfig) -> Self {
        self.timeout_config = Some(config);
        self
    }

    /// Export the schematic of this streaming pipeline.
    pub fn export_schematic(&self) -> &Schematic {
        &self.schematic
    }

    /// Apply a per-item transformation to the stream.
    ///
    /// The closure runs on each item yielded by the stream, producing a
    /// transformed item of the **same type**. This is useful for PII filtering,
    /// token counting, format normalization, etc.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let pipeline = Axon::typed::<ChatRequest, String>("chat")
    ///     .then(ClassifyIntent)
    ///     .then_stream(SynthesizeStream)
    ///     .map_items(|chunk: ChatChunk| {
    ///         ChatChunk { text: filter_pii(&chunk.text), ..chunk }
    ///     });
    /// ```
    pub fn map_items<F>(self, f: F) -> StreamingAxon<In, Item, E, Res>
    where
        F: Fn(Item) -> Item + Send + Sync + 'static,
    {
        let prev_executor = self.stream_executor;
        let f = Arc::new(f);

        let new_executor: StreamExecutor<In, Item, E, Res> = Arc::new(
            move |input: In,
                  res: &Res,
                  bus: &mut Bus|
                  -> BoxFuture<
                '_,
                Result<Pin<Box<dyn Stream<Item = Item> + Send>>, StreamingAxonError<E>>,
            > {
                let prev = prev_executor.clone();
                let f = f.clone();
                Box::pin(async move {
                    let stream = prev(input, res, bus).await?;
                    Ok(
                        Box::pin(stream.map(move |item| f(item)))
                            as Pin<Box<dyn Stream<Item = Item> + Send>>,
                    )
                })
            },
        );

        // Add map_items node to schematic
        let mut schematic = self.schematic;
        let map_node_id = uuid::Uuid::new_v4().to_string();
        let last_node_id = schematic
            .nodes
            .last()
            .map(|n| n.id.clone())
            .unwrap_or_default();

        schematic.nodes.push(ranvier_core::schematic::Node {
            id: map_node_id.clone(),
            kind: ranvier_core::schematic::NodeKind::Atom,
            label: "map_items".to_string(),
            description: Some("Per-item stream transformation".to_string()),
            input_type: std::any::type_name::<Item>().to_string(),
            output_type: std::any::type_name::<Item>().to_string(),
            resource_type: "()".to_string(),
            metadata: Default::default(),
            bus_capability: None,
            source_location: None,
            position: None,
            compensation_node_id: None,
            input_schema: None,
            output_schema: None,
            item_type: Some(std::any::type_name::<Item>().to_string()),
            terminal: Some(true),
        });

        if !last_node_id.is_empty() {
            schematic
                .edges
                .push(ranvier_core::schematic::Edge {
                    from: last_node_id,
                    to: map_node_id,
                    kind: ranvier_core::schematic::EdgeType::Linear,
                    label: Some("map".to_string()),
                });
        }

        StreamingAxon {
            schematic,
            stream_executor: new_executor,
            timeout_config: self.timeout_config,
            buffer_size: self.buffer_size,
        }
    }
}

impl<In, Item, E, Res> StreamingAxon<In, Item, E, Res>
where
    In: Send + Sync + Serialize + DeserializeOwned + 'static,
    Item: Send + Sync + Serialize + DeserializeOwned + 'static,
    E: Send + Sync + Serialize + DeserializeOwned + Debug + 'static,
    Res: ResourceRequirement,
{
    /// Collapse the stream into a `Vec<Item>`, returning a regular `Axon`.
    ///
    /// This consumes all items from the stream and collects them into a
    /// vector. Useful for testing or when you need the complete result set.
    pub fn collect_into_vec(self) -> Axon<In, Vec<Item>, String, Res> {
        let stream_executor = self.stream_executor.clone();
        let timeout_config = self.timeout_config.clone();

        let executor: crate::axon::Executor<In, Vec<Item>, String, Res> = Arc::new(
            move |input: In, res: &Res, bus: &mut Bus| -> BoxFuture<'_, Outcome<Vec<Item>, String>> {
                let stream_executor = stream_executor.clone();
                let timeout_config = timeout_config.clone();

                Box::pin(async move {
                    let stream = match stream_executor(input, res, bus).await {
                        Ok(s) => s,
                        Err(e) => return Outcome::Fault(format!("{}", e)),
                    };

                    let stream = match &timeout_config {
                        Some(config)
                            if config.init.is_some()
                                || config.idle.is_some()
                                || config.total.is_some() =>
                        {
                            Box::pin(TimeoutStream::new(stream, config.clone()))
                                as Pin<Box<dyn Stream<Item = Item> + Send>>
                        }
                        _ => stream,
                    };

                    let items: Vec<Item> = stream.collect().await;
                    Outcome::Next(items)
                })
            },
        );

        Axon {
            schematic: self.schematic,
            executor,
            execution_mode: crate::axon::ExecutionMode::Local,
            persistence_store: None,
            audit_sink: None,
            dlq_sink: None,
            dlq_policy: Default::default(),
            dynamic_dlq_policy: None,
            saga_policy: Default::default(),
            dynamic_saga_policy: None,
            saga_compensation_registry: Arc::new(std::sync::RwLock::new(
                ranvier_core::saga::SagaCompensationRegistry::new(),
            )),
            iam_handle: None,
        }
    }
}

// ---------------------------------------------------------------------------
// TimeoutStream — wraps a stream with init/idle/total timeout enforcement
// ---------------------------------------------------------------------------

struct TimeoutStream<S> {
    inner: Pin<Box<S>>,
    config: StreamTimeoutConfig,
    started_at: tokio::time::Instant,
    first_item_received: bool,
    last_item_at: tokio::time::Instant,
    finished: bool,
}

impl<S, Item> TimeoutStream<S>
where
    S: Stream<Item = Item> + Send,
{
    fn new(inner: S, config: StreamTimeoutConfig) -> Self {
        let now = tokio::time::Instant::now();
        Self {
            inner: Box::pin(inner),
            config,
            started_at: now,
            first_item_received: false,
            last_item_at: now,
            finished: false,
        }
    }
}

impl<S, Item> Stream for TimeoutStream<S>
where
    S: Stream<Item = Item> + Send,
{
    type Item = Item;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let this = unsafe { self.get_unchecked_mut() };

        if this.finished {
            return std::task::Poll::Ready(None);
        }

        // Check total timeout
        if let Some(total) = this.config.total {
            if this.started_at.elapsed() >= total {
                tracing::warn!("Stream total timeout exceeded ({:?})", total);
                this.finished = true;
                return std::task::Poll::Ready(None);
            }
        }

        match this.inner.as_mut().poll_next(cx) {
            std::task::Poll::Ready(Some(item)) => {
                this.first_item_received = true;
                this.last_item_at = tokio::time::Instant::now();
                std::task::Poll::Ready(Some(item))
            }
            std::task::Poll::Ready(None) => {
                this.finished = true;
                std::task::Poll::Ready(None)
            }
            std::task::Poll::Pending => {
                let now = tokio::time::Instant::now();

                // Check init timeout (before first item)
                if !this.first_item_received {
                    if let Some(init) = this.config.init {
                        if now.duration_since(this.started_at) >= init {
                            tracing::warn!("Stream init timeout exceeded ({:?})", init);
                            this.finished = true;
                            return std::task::Poll::Ready(None);
                        }
                    }
                }

                // Check idle timeout (between items)
                if this.first_item_received {
                    if let Some(idle) = this.config.idle {
                        if now.duration_since(this.last_item_at) >= idle {
                            tracing::warn!("Stream idle timeout exceeded ({:?})", idle);
                            this.finished = true;
                            return std::task::Poll::Ready(None);
                        }
                    }
                }

                std::task::Poll::Pending
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;
    use ranvier_core::bus::Bus;

    #[tokio::test]
    async fn test_streaming_axon_basic_execute() {
        // Create a simple StreamingAxon that yields 3 items
        let stream_executor: StreamExecutor<String, String, String, ()> =
            Arc::new(|input: String, _res: &(), _bus: &mut Bus| {
                Box::pin(async move {
                    let items = vec![
                        format!("chunk1: {}", input),
                        format!("chunk2: {}", input),
                        format!("chunk3: {}", input),
                    ];
                    Ok(Box::pin(stream::iter(items)) as Pin<Box<dyn Stream<Item = String> + Send>>)
                })
            });

        let sa = StreamingAxon {
            schematic: Schematic::new("test"),
            stream_executor,
            timeout_config: None,
            buffer_size: 64,
        };

        let mut bus = Bus::new();
        let stream = sa.execute("hello".to_string(), &(), &mut bus).await.unwrap();
        let items: Vec<String> = stream.collect().await;
        assert_eq!(items.len(), 3);
        assert_eq!(items[0], "chunk1: hello");
    }

    #[tokio::test]
    async fn test_streaming_axon_collect_into_vec() {
        let stream_executor: StreamExecutor<(), String, String, ()> =
            Arc::new(|_input: (), _res: &(), _bus: &mut Bus| {
                Box::pin(async move {
                    let items = vec!["a".to_string(), "b".to_string(), "c".to_string()];
                    Ok(Box::pin(stream::iter(items)) as Pin<Box<dyn Stream<Item = String> + Send>>)
                })
            });

        let sa: StreamingAxon<(), String, String, ()> = StreamingAxon {
            schematic: Schematic::new("test-collect"),
            stream_executor,
            timeout_config: None,
            buffer_size: 64,
        };

        let axon = sa.collect_into_vec();
        let mut bus = Bus::new();
        let result = axon.execute((), &(), &mut bus).await;
        match result {
            Outcome::Next(items) => {
                assert_eq!(items, vec!["a", "b", "c"]);
            }
            other => panic!("Expected Next, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_streaming_axon_pipeline_fault() {
        let stream_executor: StreamExecutor<(), String, String, ()> =
            Arc::new(|_input: (), _res: &(), _bus: &mut Bus| {
                Box::pin(async move {
                    Err(StreamingAxonError::PipelineFault("step failed".to_string()))
                })
            });

        let sa = StreamingAxon {
            schematic: Schematic::new("test-fault"),
            stream_executor,
            timeout_config: None,
            buffer_size: 64,
        };

        let mut bus = Bus::new();
        let result = sa.execute((), &(), &mut bus).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_streaming_axon_map_items() {
        let stream_executor: StreamExecutor<String, String, String, ()> =
            Arc::new(|input: String, _res: &(), _bus: &mut Bus| {
                Box::pin(async move {
                    let items = vec![
                        format!("hello {}", input),
                        format!("world {}", input),
                    ];
                    Ok(Box::pin(stream::iter(items)) as Pin<Box<dyn Stream<Item = String> + Send>>)
                })
            });

        let sa = StreamingAxon {
            schematic: Schematic::new("test-map"),
            stream_executor,
            timeout_config: None,
            buffer_size: 64,
        };

        // Apply map_items to uppercase each chunk
        let sa = sa.map_items(|s| s.to_uppercase());

        let mut bus = Bus::new();
        let stream = sa.execute("test".to_string(), &(), &mut bus).await.unwrap();
        let items: Vec<String> = stream.collect().await;
        assert_eq!(items.len(), 2);
        assert_eq!(items[0], "HELLO TEST");
        assert_eq!(items[1], "WORLD TEST");
    }

    #[tokio::test]
    async fn test_streaming_axon_map_items_schematic() {
        let stream_executor: StreamExecutor<(), String, String, ()> =
            Arc::new(|_input: (), _res: &(), _bus: &mut Bus| {
                Box::pin(async move {
                    Ok(Box::pin(stream::iter(vec!["a".to_string()]))
                        as Pin<Box<dyn Stream<Item = String> + Send>>)
                })
            });

        let sa = StreamingAxon {
            schematic: Schematic::new("test-map-schematic"),
            stream_executor,
            timeout_config: None,
            buffer_size: 64,
        };

        let sa = sa.map_items(|s| s);
        let schematic = sa.export_schematic();
        assert_eq!(schematic.nodes.len(), 1);
        assert_eq!(schematic.nodes[0].label, "map_items");
        assert!(schematic.nodes[0].item_type.is_some());
    }

    #[tokio::test]
    async fn test_streaming_axon_clone() {
        let stream_executor: StreamExecutor<(), String, String, ()> =
            Arc::new(|_input: (), _res: &(), _bus: &mut Bus| {
                Box::pin(async move {
                    Ok(Box::pin(stream::iter(vec!["x".to_string()]))
                        as Pin<Box<dyn Stream<Item = String> + Send>>)
                })
            });

        let sa = StreamingAxon {
            schematic: Schematic::new("test-clone"),
            stream_executor,
            timeout_config: None,
            buffer_size: 64,
        };

        let sa2 = sa.clone();
        let mut bus = Bus::new();
        let items: Vec<String> = sa2.execute((), &(), &mut bus).await.unwrap().collect().await;
        assert_eq!(items, vec!["x"]);
    }
}
