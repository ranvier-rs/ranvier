use bytes::Bytes;
use futures_util::Stream;
use http_body_util::BodyExt;
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::response::{RanvierResponse, IntoResponse};
use http::header::{CACHE_CONTROL, CONTENT_TYPE};
use http::{HeaderValue, Response, StatusCode};
use tokio::sync::mpsc;
use hyper::body::{Body, Frame};

/// A Server-Sent Event frame.
#[derive(Debug, Clone)]
pub struct SseEvent {
    pub id: Option<String>,
    pub event: Option<String>,
    pub data: String,
    pub retry: Option<u64>,
}

impl SseEvent {
    pub fn data(data: impl Into<String>) -> Self {
        Self {
            id: None,
            event: None,
            data: data.into(),
            retry: None,
        }
    }

    pub fn event(mut self, event: impl Into<String>) -> Self {
        self.event = Some(event.into());
        self
    }

    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    pub fn retry(mut self, retry: u64) -> Self {
        self.retry = Some(retry);
        self
    }

    pub fn to_string(&self) -> String {
        let mut s = String::new();
        if let Some(ref id) = self.id {
            s.push_str("id: ");
            s.push_str(id);
            s.push('\n');
        }
        if let Some(ref event) = self.event {
            s.push_str("event: ");
            s.push_str(event);
            s.push('\n');
        }
        for line in self.data.lines() {
            s.push_str("data: ");
            s.push_str(line);
            s.push('\n');
        }
        if let Some(retry) = self.retry {
            s.push_str("retry: ");
            s.push_str(&retry.to_string());
            s.push('\n');
        }
        s.push('\n');
        s
    }
}

/// A stream of SSE events wrapped as an HTTP body.
pub struct SseBody<S> {
    stream: S,
}

impl<S> SseBody<S> {
    pub fn new(stream: S) -> Self {
        Self { stream }
    }
}

impl<S, E> Stream for SseBody<S>
where
    S: Stream<Item = Result<SseEvent, E>> + Unpin,
{
    type Item = Result<Frame<Bytes>, E>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.stream).poll_next(cx) {
            Poll::Ready(Some(Ok(event))) => {
                Poll::Ready(Some(Ok(Frame::data(Bytes::from(event.to_string())))))
            }
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S, E> Body for SseBody<S>
where
    S: Stream<Item = Result<SseEvent, E>> + Unpin + Send + Sync + 'static,
    E: std::fmt::Debug + Send + Sync + 'static,
{
    type Data = Bytes;
    type Error = std::convert::Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match Pin::new(&mut self.stream).poll_next(cx) {
            Poll::Ready(Some(Ok(event))) => {
                Poll::Ready(Some(Ok(Frame::data(Bytes::from(event.to_string())))))
            }
            Poll::Ready(Some(Err(e))) => {
                tracing::error!("SSE stream error: {:?}", e);
                Poll::Ready(None) // End stream on error to stay infallible
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Helper to convert an SSE stream into a RanvierResponse.
pub fn sse_response<S, E>(stream: S) -> RanvierResponse
where
    S: Stream<Item = Result<SseEvent, E>> + Send + Sync + Unpin + 'static,
    E: std::fmt::Debug + Send + Sync + 'static,
{
    let body = SseBody::new(stream);
    let boxed_body = body.boxed();

    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"))
        .header(CACHE_CONTROL, HeaderValue::from_static("no-cache"))
        .body(boxed_body)
        .unwrap()
}

/// Newtype to allow implementing IntoResponse for arbitrary SSE streams.
pub struct Sse<S>(pub S);

impl<S, E> IntoResponse for Sse<S>
where
    S: Stream<Item = Result<SseEvent, E>> + Send + Sync + Unpin + 'static,
    E: std::fmt::Debug + Send + Sync + 'static,
{
    fn into_response(self) -> RanvierResponse {
        sse_response(self.0)
    }
}

/// A sink for sending SSE events from within an Axon transition.
#[derive(Debug, Clone)]
pub struct SseSink {
    sender: mpsc::Sender<SseEvent>,
}

impl SseSink {
    pub fn new(sender: mpsc::Sender<SseEvent>) -> Self {
        Self { sender }
    }

    pub async fn emit(&self, event: SseEvent) -> Result<(), mpsc::error::SendError<SseEvent>> {
        self.sender.send(event).await
    }
}

/// Bus-injectable capability for SSE.
#[derive(Debug, Clone)]
pub struct SseInject {
    pub sink: SseSink,
}

impl SseInject {
    pub fn new(sink: SseSink) -> Self {
        Self { sink }
    }
}
