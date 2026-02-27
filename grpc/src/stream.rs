//! gRPC streaming utilities for Ranvier.
//!
//! Bridges Ranvier's `EventSource` pattern to tonic's streaming model,
//! supporting server streaming, client streaming, and bi-directional streaming.

use futures_core::Stream;
use std::pin::Pin;

/// Type alias for a pinned, boxed stream compatible with tonic's streaming responses.
///
/// This is the standard return type for server-streaming and bi-directional RPC methods.
pub type GrpcStream<T> = Pin<Box<dyn Stream<Item = Result<T, tonic::Status>> + Send + 'static>>;

/// Create a `GrpcStream` from an `EventSource`-like async iterator.
///
/// This bridges the Ranvier `EventSource` pattern to tonic's streaming model
/// using a `tokio::mpsc` channel to decouple the source from the stream consumer.
///
/// # Example
///
/// ```rust,ignore
/// use ranvier_grpc::stream::from_event_stream;
///
/// let stream = from_event_stream(16, |tx| async move {
///     for i in 0..10 {
///         if tx.send(Ok(MyResponse { value: i })).await.is_err() {
///             break;
///         }
///     }
/// });
/// ```
pub fn from_event_stream<T, F, Fut>(buffer_size: usize, producer: F) -> GrpcStream<T>
where
    T: Send + 'static,
    F: FnOnce(tokio::sync::mpsc::Sender<Result<T, tonic::Status>>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let (tx, rx) = tokio::sync::mpsc::channel(buffer_size);

    tokio::spawn(async move {
        producer(tx).await;
    });

    Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx))
}

/// Create a `GrpcStream` from an iterator of items (useful for testing or finite responses).
pub fn from_iter<T, I>(items: I) -> GrpcStream<T>
where
    T: Send + 'static,
    I: IntoIterator<Item = Result<T, tonic::Status>> + Send + 'static,
    I::IntoIter: Send,
{
    let stream = futures_util::stream::iter(items);
    Box::pin(stream)
}
