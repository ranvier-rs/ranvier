use crate::response::{HttpResponse, IntoResponse};
use bytes::Bytes;
use futures_util::stream::Stream;

use ranvier_core::event::EventSource;
use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub(crate) data: Option<String>,
    pub(crate) id: Option<String>,
    pub(crate) event: Option<String>,
    pub(crate) retry: Option<Duration>,
    pub(crate) comment: Option<String>,
}

impl SseEvent {
    pub fn default() -> Self {
        Self {
            data: None,
            id: None,
            event: None,
            retry: None,
            comment: None,
        }
    }

    pub fn data(mut self, data: impl Into<String>) -> Self {
        self.data = Some(data.into());
        self
    }

    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    pub fn event(mut self, event: impl Into<String>) -> Self {
        self.event = Some(event.into());
        self
    }

    pub fn retry(mut self, duration: Duration) -> Self {
        self.retry = Some(duration);
        self
    }

    pub fn comment(mut self, comment: impl Into<String>) -> Self {
        self.comment = Some(comment.into());
        self
    }

    fn serialize(&self) -> String {
        let mut out = String::new();
        if let Some(comment) = &self.comment {
            for line in comment.lines() {
                out.push_str(&format!(": {}\n", line));
            }
        }
        if let Some(event) = &self.event {
            out.push_str(&format!("event: {}\n", event));
        }
        if let Some(id) = &self.id {
            out.push_str(&format!("id: {}\n", id));
        }
        if let Some(retry) = &self.retry {
            out.push_str(&format!("retry: {}\n", retry.as_millis()));
        }
        if let Some(data) = &self.data {
            for line in data.lines() {
                out.push_str(&format!("data: {}\n", line));
            }
        }
        out.push('\n');
        out
    }
}

pub struct Sse<S> {
    stream: S,
}

impl<S, E> Sse<S>
where
    S: Stream<Item = Result<SseEvent, E>> + Send + 'static,
    E: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    pub fn new(stream: S) -> Self {
        Self { stream }
    }
}

pub struct FrameStream<S, E> {
    inner: S,
    _marker: std::marker::PhantomData<fn() -> E>,
}

impl<S, E> Stream for FrameStream<S, E>
where
    S: Stream<Item = Result<SseEvent, E>> + Unpin,
    E: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    type Item = Result<http_body::Frame<Bytes>, E>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<<Self as Stream>::Item>> {
        match Pin::new(&mut self.inner).poll_next(cx) {
            Poll::Ready(Some(Ok(event))) => {
                let serialized = event.serialize();
                let frame = http_body::Frame::data(Bytes::from(serialized));
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S, E> IntoResponse for Sse<S>
where
    S: Stream<Item = Result<SseEvent, E>> + Send + Sync + Unpin + 'static,
    E: Into<Box<dyn std::error::Error + Send + Sync>> + Send + Sync + 'static,
{
    fn into_response(self) -> HttpResponse {
        let frame_stream = FrameStream {
            inner: self.stream,
            _marker: std::marker::PhantomData,
        };

        let mut frame_stream = Box::pin(frame_stream);
        let infallible_stream = async_stream::stream! {
            while let Some(res) = futures_util::StreamExt::next(&mut frame_stream).await {
                match res {
                    Ok(frame) => yield Ok::<_, std::convert::Infallible>(frame),
                    Err(e) => {
                        let err: Box<dyn std::error::Error + Send + Sync> = e.into();
                        tracing::error!("SSE stream terminated with error: {:?}", err);
                        break;
                    }
                }
            }
        };

        let body = http_body_util::StreamBody::new(infallible_stream);

        http::Response::builder()
            .status(http::StatusCode::OK)
            .header(http::header::CONTENT_TYPE, "text/event-stream")
            .header(http::header::CACHE_CONTROL, "no-cache")
            .header(http::header::CONNECTION, "keep-alive")
            .body(http_body_util::BodyExt::boxed(body))
            .expect("Valid builder")
    }
}

pub fn from_event_source<E, S, F>(
    mut source: S,
    mut mapper: F,
) -> impl Stream<Item = Result<SseEvent, Infallible>> + Send + Sync
where
    S: EventSource<E> + Send + 'static,
    E: Send + 'static,
    F: FnMut(E) -> SseEvent + Send + 'static,
{
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    tokio::spawn(async move {
        while let Some(event) = source.next_event().await {
            if tx.send(mapper(event)).await.is_err() {
                break;
            }
        }
    });

    let stream = async_stream::stream! {
        while let Some(event) = rx.recv().await {
            yield Ok(event);
        }
    };
    Box::pin(stream)
}
