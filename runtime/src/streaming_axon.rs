//! Report-aware execution of a configured Axon prefix and terminal stream initializer.
//! Prefix acknowledgments do not certify stream items or late completion.
use crate::axon::BoxFuture;
use crate::streaming_initialization::StreamInitializationReport;
use futures_core::Stream;
use futures_util::StreamExt;
use ranvier_core::schematic::Schematic;
use ranvier_core::streaming::StreamTimeoutConfig;
use ranvier_core::{Bus, CancellationContext, CancellationToken};
use std::{fmt::Debug, pin::Pin, sync::Arc};

/// Terminal stream with its typed prefix output retained for rejected execution.
pub struct StreamingAxon<In, Out, Item, E, Res = ()> {
    pub schematic: Schematic,
    pub(crate) stream_executor: StreamExecutorType<In, Out, Item, E, Res>,
    pub timeout_config: Option<StreamTimeoutConfig>,
    pub buffer_size: usize,
}

pub type StreamExecutorType<In, Out, Item, E, Res> = Arc<
    dyn for<'a> Fn(
            In,
            &'a Res,
            &'a mut Bus,
            Option<CancellationToken>,
        ) -> BoxFuture<
            'a,
            StreamInitializationReport<Out, Pin<Box<dyn Stream<Item = Item> + Send>>, E>,
        > + Send
        + Sync,
>;

/// Initialization failures are separate from retained prefix outcomes.
#[derive(Debug)]
pub enum StreamingAxonError<E> {
    IamRejected(&'static str),
    PipelineFault(E),
    UnexpectedOutcome(String),
    StreamInitError(String),
    Timeout(StreamTimeoutKind),
}
#[derive(Debug)]
pub enum CancellableStreamingError<E> {
    Stream(StreamingAxonError<E>),
    Cancelled(CancellationContext),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamTimeoutKind {
    Init,
    Idle,
    Total,
}
impl<E: Debug> std::fmt::Display for StreamingAxonError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IamRejected(event) => write!(f, "IAM rejected: {event}"),
            Self::PipelineFault(error) => write!(f, "Pipeline fault: {error:?}"),
            Self::UnexpectedOutcome(message) => write!(f, "Unexpected outcome: {message}"),
            Self::StreamInitError(message) => write!(f, "Stream init error: {message}"),
            Self::Timeout(kind) => write!(f, "Stream timeout: {kind:?}"),
        }
    }
}
impl<E: Debug> std::error::Error for StreamingAxonError<E> {}
impl<E: Debug> std::fmt::Display for CancellableStreamingError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stream(error) => write!(f, "{error}"),
            Self::Cancelled(context) => write!(f, "stream cancelled: {:?}", context.reason),
        }
    }
}
impl<E: Debug> std::error::Error for CancellableStreamingError<E> {}
impl<E> From<StreamingAxonError<E>> for CancellableStreamingError<E> {
    fn from(error: StreamingAxonError<E>) -> Self {
        Self::Stream(error)
    }
}
impl<In, Out, Item, E, Res> Clone for StreamingAxon<In, Out, Item, E, Res> {
    fn clone(&self) -> Self {
        Self {
            schematic: self.schematic.clone(),
            stream_executor: self.stream_executor.clone(),
            timeout_config: self.timeout_config.clone(),
            buffer_size: self.buffer_size,
        }
    }
}
impl<In, Out, Item, E, Res> StreamingAxon<In, Out, Item, E, Res>
where
    In: Send + Sync + 'static,
    Out: Send + Sync + 'static,
    Item: Send + 'static,
    E: Send + Sync + Debug + 'static,
    Res: Send + Sync + 'static,
{
    /// Execute and retain prefix durability, typed rejection and initialization evidence.
    pub async fn execute(
        &self,
        input: In,
        resources: &Res,
        bus: &mut Bus,
    ) -> StreamInitializationReport<Out, Pin<Box<dyn Stream<Item = Item> + Send>>, E> {
        let report = (self.stream_executor)(input, resources, bus, None).await;
        report.map_value(|stream| self.wrap_stream(stream, None))
    }
    /// Cancellation inside the prefix waits for its report-aware shielded cleanup.
    /// Cancellation after initialization ends the owned stream independently of caller Bus.
    pub async fn execute_cancellable(
        &self,
        input: In,
        resources: &Res,
        bus: &mut Bus,
        token: CancellationToken,
    ) -> StreamInitializationReport<Out, Pin<Box<dyn Stream<Item = Item> + Send>>, E> {
        let report = (self.stream_executor)(input, resources, bus, Some(token.clone())).await;
        report.map_value(|stream| self.wrap_stream(stream, Some(token)))
    }
    fn wrap_stream(
        &self,
        stream: Pin<Box<dyn Stream<Item = Item> + Send>>,
        token: Option<CancellationToken>,
    ) -> Pin<Box<dyn Stream<Item = Item> + Send>> {
        let stream: Pin<Box<dyn Stream<Item = Item> + Send>> = match &self.timeout_config {
            Some(config)
                if config.init.is_some() || config.idle.is_some() || config.total.is_some() =>
            {
                Box::pin(TimeoutStream::new(stream, config.clone()))
            }
            _ => stream,
        };
        match token {
            Some(token) => Box::pin(CancellationStream::new(stream, token)),
            None => stream,
        }
    }
    pub fn with_buffer_size(mut self, size: usize) -> Self {
        self.buffer_size = size;
        self
    }
    pub fn with_timeout(mut self, config: StreamTimeoutConfig) -> Self {
        self.timeout_config = Some(config);
        self
    }
    pub fn export_schematic(&self) -> &Schematic {
        &self.schematic
    }

    /// Transform items only in Ready; rejected prefix results retain their original type.
    pub fn map_items<F>(self, f: F) -> Self
    where
        F: Fn(Item) -> Item + Send + Sync + 'static,
    {
        let previous = self.stream_executor;
        let f = Arc::new(f);
        let executor: StreamExecutorType<In, Out, Item, E, Res> =
            Arc::new(move |input, resources, bus, token| {
                let previous = previous.clone();
                let f = f.clone();
                Box::pin(async move {
                    previous(input, resources, bus, token)
                        .await
                        .map_value(|stream| {
                            Box::pin(stream.map(move |item| f(item)))
                                as Pin<Box<dyn Stream<Item = Item> + Send>>
                        })
                })
            });
        let mut schematic = self.schematic;
        let node_id = uuid::Uuid::new_v4().to_string();
        let last = schematic.nodes.last().map(|node| node.id.clone());
        schematic.nodes.push(ranvier_core::schematic::Node {
            id: node_id.clone(),
            kind: ranvier_core::schematic::NodeKind::Atom,
            label: "map_items".into(),
            description: Some("Per-item stream transformation".into()),
            input_type: std::any::type_name::<Item>().into(),
            output_type: std::any::type_name::<Item>().into(),
            resource_type: "()".into(),
            metadata: Default::default(),
            bus_capability: None,
            source_location: None,
            position: None,
            compensation_node_id: None,
            input_schema: None,
            output_schema: None,
            item_type: Some(std::any::type_name::<Item>().into()),
            terminal: Some(true),
        });
        if let Some(last) = last {
            schematic.edges.push(ranvier_core::schematic::Edge {
                from: last,
                to: node_id,
                kind: ranvier_core::schematic::EdgeType::Linear,
                label: Some("map".into()),
            });
        }
        Self {
            schematic,
            stream_executor: executor,
            timeout_config: self.timeout_config,
            buffer_size: self.buffer_size,
        }
    }
    /// Collect Ready items while retaining the prefix report. This is a terminal
    /// collection owner, not an ordinary Axon whose raw executor discards evidence.
    pub fn collect_into_vec(self) -> CollectedStreamingAxon<In, Out, Item, E, Res> {
        CollectedStreamingAxon { streaming: self }
    }
}

pub struct CollectedStreamingAxon<In, Out, Item, E, Res = ()> {
    streaming: StreamingAxon<In, Out, Item, E, Res>,
}
impl<In, Out, Item, E, Res> CollectedStreamingAxon<In, Out, Item, E, Res>
where
    In: Send + Sync + 'static,
    Out: Send + Sync + 'static,
    Item: Send + 'static,
    E: Send + Sync + Debug + 'static,
    Res: Send + Sync + 'static,
{
    pub fn export_schematic(&self) -> &Schematic {
        self.streaming.export_schematic()
    }
    pub async fn execute(
        &self,
        input: In,
        resources: &Res,
        bus: &mut Bus,
    ) -> StreamInitializationReport<Out, Vec<Item>, E> {
        match self.streaming.execute(input, resources, bus).await {
            StreamInitializationReport::Ready { prefix, value } => {
                StreamInitializationReport::Ready {
                    prefix,
                    value: value.collect().await,
                }
            }
            report => report.map_value(|_| unreachable!("Ready is handled above")),
        }
    }
}

struct CancellationStream<Item> {
    inner: Pin<Box<dyn Stream<Item = Item> + Send>>,
    cancellation: Pin<Box<dyn std::future::Future<Output = CancellationContext> + Send>>,
    finished: bool,
}

impl<Item> CancellationStream<Item> {
    fn new(inner: Pin<Box<dyn Stream<Item = Item> + Send>>, token: CancellationToken) -> Self {
        let cancellation = Box::pin(async move { token.cancelled().await });
        Self {
            inner,
            cancellation,
            finished: false,
        }
    }
}

impl<Item> Stream for CancellationStream<Item> {
    type Item = Item;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.finished {
            return std::task::Poll::Ready(None);
        }

        if this.cancellation.as_mut().poll(cx).is_ready() {
            this.finished = true;
            return std::task::Poll::Ready(None);
        }

        this.inner.as_mut().poll_next(cx)
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
    use crate::{Axon, ExecutionReport, ExecutionTerminal};
    use ranvier_core::{Outcome, streaming::StreamingTransition};
    #[derive(Clone)]
    struct Chunks {
        items: Vec<String>,
        pending: bool,
        include_input: bool,
    }
    #[async_trait::async_trait]
    impl StreamingTransition<String> for Chunks {
        type Item = String;
        type Error = String;
        type Resources = ();
        async fn run_stream(
            &self,
            input: String,
            _: &(),
            _: &mut Bus,
        ) -> Result<Pin<Box<dyn Stream<Item = String> + Send>>, String> {
            if self.pending {
                Ok(Box::pin(futures_util::stream::pending()))
            } else {
                let items: Vec<_> = self
                    .items
                    .iter()
                    .map(|item| {
                        if self.include_input {
                            format!("{item}{input}")
                        } else {
                            item.clone()
                        }
                    })
                    .collect();
                Ok(Box::pin(futures_util::stream::iter(items)))
            }
        }
    }
    fn circuit(items: &[&str]) -> StreamingAxon<String, String, String, String> {
        Axon::<String, String, String>::start("test").then_stream(Chunks {
            items: items.iter().map(|item| item.to_string()).collect(),
            pending: false,
            include_input: false,
        })
    }
    fn ready(
        report: StreamInitializationReport<
            String,
            Pin<Box<dyn Stream<Item = String> + Send>>,
            String,
        >,
    ) -> Pin<Box<dyn Stream<Item = String> + Send>> {
        match report {
            StreamInitializationReport::Ready {
                prefix:
                    ExecutionReport::Unpersisted {
                        result: (),
                        cleanup: None,
                    },
                value,
            } => value,
            _ => panic!("expected unpersisted initialized stream"),
        }
    }
    #[tokio::test]
    async fn test_streaming_axon_basic_execute() {
        let circuit = Axon::<String, String, String>::start("test").then_stream(Chunks {
            items: vec!["chunk1: ".into(), "chunk2: ".into(), "chunk3: ".into()],
            pending: false,
            include_input: true,
        });
        let items = ready(circuit.execute("hello".into(), &(), &mut Bus::new()).await)
            .collect::<Vec<_>>()
            .await;
        assert_eq!(items.len(), 3);
        assert_eq!(items[0], "chunk1: hello");
    }
    #[tokio::test]
    async fn test_streaming_axon_collect_into_vec() {
        let report = circuit(&["a", "b", "c"])
            .collect_into_vec()
            .execute("".into(), &(), &mut Bus::new())
            .await;
        match report {
            StreamInitializationReport::Ready {
                prefix:
                    ExecutionReport::Unpersisted {
                        result: (),
                        cleanup: None,
                    },
                value,
            } => assert_eq!(value, vec!["a", "b", "c"]),
            _ => panic!("collection lost initialization report"),
        }
    }
    #[derive(Clone)]
    struct Fault;
    #[async_trait::async_trait]
    impl ranvier_core::Transition<String, String> for Fault {
        type Error = String;
        type Resources = ();
        async fn run(&self, _: String, _: &(), _: &mut Bus) -> Outcome<String, String> {
            Outcome::Fault("step failed".into())
        }
    }
    #[tokio::test]
    async fn test_streaming_axon_pipeline_fault() {
        let circuit = Axon::<String, String, String>::start("fault")
            .then(Fault)
            .then_stream(Chunks {
                items: vec![],
                pending: false,
                include_input: false,
            });
        assert!(
            matches!(circuit.execute("".into(), &(), &mut Bus::new()).await,
            StreamInitializationReport::Rejected { prefix: ExecutionReport::Unpersisted {
                result: ExecutionTerminal::Outcome(Outcome::Fault(ref error)), .. } } if error == "step failed")
        );
    }
    #[tokio::test]
    async fn test_streaming_axon_map_items() {
        let circuit =
            circuit(&["hello test", "world test"]).map_items(|value| value.to_uppercase());
        let items = ready(circuit.execute("".into(), &(), &mut Bus::new()).await)
            .collect::<Vec<_>>()
            .await;
        assert_eq!(items, vec!["HELLO TEST", "WORLD TEST"]);
    }
    #[tokio::test]
    async fn test_streaming_axon_map_items_schematic() {
        let circuit = circuit(&["a"]).map_items(|value| value);
        let node = circuit.export_schematic().nodes.last().unwrap();
        assert_eq!(node.label, "map_items");
        assert!(node.item_type.is_some());
    }
    #[tokio::test]
    async fn test_streaming_axon_clone() {
        let cloned = circuit(&["x"]).clone();
        let items = ready(cloned.execute("".into(), &(), &mut Bus::new()).await)
            .collect::<Vec<_>>()
            .await;
        assert_eq!(items, vec!["x"]);
    }
    #[tokio::test]
    async fn cancellation_can_win_stream_initialization() {
        let token = CancellationToken::new();
        token.cancel(ranvier_core::CancellationReason::Explicit);
        assert!(matches!(
            circuit(&[])
                .execute_cancellable("".into(), &(), &mut Bus::new(), token)
                .await,
            StreamInitializationReport::Rejected {
                prefix: ExecutionReport::Unpersisted {
                    result: ExecutionTerminal::Cancelled(_),
                    ..
                }
            }
        ));
    }
    #[tokio::test]
    async fn cancellation_ends_initialized_stream_between_items() {
        let circuit = Axon::<String, String, String>::start("pending").then_stream(Chunks {
            items: vec![],
            pending: true,
            include_input: false,
        });
        let token = CancellationToken::new();
        let mut value = ready(
            circuit
                .execute_cancellable("".into(), &(), &mut Bus::new(), token.clone())
                .await,
        );
        token.cancel(ranvier_core::CancellationReason::Explicit);
        assert_eq!(value.next().await, None);
    }
}
