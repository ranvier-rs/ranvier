#![cfg(feature = "streaming")]
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper_util::rt::TokioIo;
use ranvier_core::{Bus, CancellationReason, CancellationToken, streaming::StreamingTransition};
use ranvier_http::prelude::HttpIngress;
use ranvier_runtime::Axon;
use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);
impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}
struct Source {
    first: bool,
    pending: Arc<tokio::sync::Notify>,
    dropped: Arc<tokio::sync::Notify>,
    drops: Arc<AtomicUsize>,
}
impl futures_util::Stream for Source {
    type Item = String;
    fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<String>> {
        let this = self.get_mut();
        if this.first {
            this.first = false;
            return Poll::Ready(Some("first".into()));
        }
        this.pending.notify_one();
        Poll::Pending
    }
}
impl Drop for Source {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
        self.dropped.notify_one();
    }
}
#[derive(Clone)]
struct Init(
    Arc<tokio::sync::Notify>,
    Arc<tokio::sync::Notify>,
    Arc<AtomicUsize>,
    bool,
);
#[async_trait::async_trait]
impl StreamingTransition<()> for Init {
    type Item = String;
    type Error = String;
    type Resources = ();
    async fn run_stream(
        &self,
        _: (),
        _: &(),
        _: &mut Bus,
    ) -> Result<Pin<Box<dyn futures_util::Stream<Item = String> + Send>>, String> {
        Ok(Box::pin(Source {
            first: self.3,
            pending: self.0.clone(),
            dropped: self.1.clone(),
            drops: self.2.clone(),
        }))
    }
}
#[tokio::test]
async fn disconnected_peer_releases_pending_stream_without_shutdown_or_timeout() {
    for first in [true, false] {
        assert_disconnect(first).await;
    }
}
/// D-STREAM-DESIGN S8: a disconnect after partial delivery is journaled as an
/// abandoned stream instead of leaving an unexplained open Intent.
#[tokio::test]
async fn disconnected_peer_records_abandoned_stream_under_configured_store() {
    use ranvier_runtime::persistence::{
        CompletionState, InMemoryPersistenceStore, PersistenceEventType, PersistenceHandle,
        PersistenceStore, PersistenceTraceId,
    };
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let pending = Arc::new(tokio::sync::Notify::new());
    let dropped = Arc::new(tokio::sync::Notify::new());
    let drops = Arc::new(AtomicUsize::new(0));
    let store = InMemoryPersistenceStore::new();
    let injected = store.clone();
    let stream = Axon::<(), (), String>::start("disconnect-stored").then_stream(Init(
        pending.clone(),
        dropped.clone(),
        drops.clone(),
        true,
    ));
    let ingress = HttpIngress::<()>::new()
        .bind(address.to_string())
        .bus_injector(move |_, bus| {
            bus.insert(PersistenceHandle::from_store(injected.clone()));
            bus.insert(PersistenceTraceId::new("sse-abandoned"));
        })
        .post_sse("/stream", stream);
    let shutdown = CancellationToken::new();
    let server_token = shutdown.clone();
    let mut server = AbortOnDrop(tokio::spawn(async move {
        ingress.run_with_cancellation((), server_token).await
    }));
    let socket = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match tokio::net::TcpStream::connect(address).await {
                Ok(socket) => break socket,
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    })
    .await
    .unwrap();
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(socket))
        .await
        .unwrap();
    let driver = AbortOnDrop(tokio::spawn(connection));
    let response = tokio::time::timeout(
        Duration::from_secs(3),
        sender.send_request(
            http::Request::builder()
                .method("POST")
                .uri("/stream")
                .header("host", "fixture")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(response.status(), 200);
    let mut body = response.into_body();
    let frame = tokio::time::timeout(Duration::from_secs(3), body.frame())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        std::str::from_utf8(frame.data_ref().unwrap())
            .unwrap()
            .contains("first")
    );
    tokio::time::timeout(Duration::from_secs(3), pending.notified())
        .await
        .unwrap();
    drop(body);
    drop(sender);
    drop(driver);
    // The end record follows source release; wait for the completion write.
    let trace = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(trace) = store.load("sse-abandoned").await.unwrap()
                && trace.completion.is_some()
            {
                break trace;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("abandoned stream end was not recorded");
    shutdown.cancel(CancellationReason::OperatorShutdown);
    let _ = tokio::time::timeout(Duration::from_secs(3), &mut server.0).await;
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert_eq!(trace.completion, Some(CompletionState::Cancelled));
    let result = trace
        .events
        .iter()
        .find(|event| {
            event.event_type == PersistenceEventType::Result
                && event.outcome_kind == "StreamAbandoned"
        })
        .expect("StreamAbandoned result");
    assert_eq!(
        result.payload,
        Some(serde_json::json!({"terminal": "abandoned", "items_observed": 1}))
    );
    assert_eq!(
        trace.events.last().map(|event| event.event_type),
        Some(PersistenceEventType::Terminal)
    );
}

async fn assert_disconnect(first: bool) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let pending = Arc::new(tokio::sync::Notify::new());
    let dropped = Arc::new(tokio::sync::Notify::new());
    let drops = Arc::new(AtomicUsize::new(0));
    let stream = Axon::<(), (), String>::start("disconnect").then_stream(Init(
        pending.clone(),
        dropped.clone(),
        drops.clone(),
        first,
    ));
    let ingress = HttpIngress::<()>::new()
        .bind(address.to_string())
        .post_sse("/stream", stream);
    let shutdown = CancellationToken::new();
    let server_token = shutdown.clone();
    let mut server = AbortOnDrop(tokio::spawn(async move {
        ingress.run_with_cancellation((), server_token).await
    }));
    let socket = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match tokio::net::TcpStream::connect(address).await {
                Ok(socket) => break socket,
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    })
    .await
    .unwrap();
    if !first {
        // Disconnect before a first item without relying on response-header flush.
        use tokio::io::AsyncWriteExt;
        let mut socket = socket;
        socket
            .write_all(b"POST /stream HTTP/1.1\r\nHost: fixture\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), pending.notified())
            .await
            .unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(socket);
        let released = tokio::time::timeout(Duration::from_secs(3), dropped.notified())
            .await
            .is_ok();
        let before_shutdown = drops.load(Ordering::SeqCst);
        shutdown.cancel(CancellationReason::OperatorShutdown);
        let _ = tokio::time::timeout(Duration::from_secs(3), &mut server.0).await;
        assert!(released, "pre-first-item disconnect leaked source");
        assert_eq!(before_shutdown, 1);
        return;
    }
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(socket))
        .await
        .unwrap();
    let driver = AbortOnDrop(tokio::spawn(connection));
    let response = tokio::time::timeout(
        Duration::from_secs(3),
        sender.send_request(
            http::Request::builder()
                .method("POST")
                .uri("/stream")
                .header("host", "fixture")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(response.status(), 200);
    let mut body = response.into_body();
    let frame = tokio::time::timeout(Duration::from_secs(3), body.frame())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        std::str::from_utf8(frame.data_ref().unwrap())
            .unwrap()
            .contains("first")
    );
    tokio::time::timeout(Duration::from_secs(3), pending.notified())
        .await
        .unwrap();
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(body);
    drop(sender);
    drop(driver);
    let released = tokio::time::timeout(Duration::from_secs(3), dropped.notified())
        .await
        .is_ok();
    let before_shutdown = drops.load(Ordering::SeqCst);
    shutdown.cancel(CancellationReason::OperatorShutdown);
    let _ = tokio::time::timeout(Duration::from_secs(3), &mut server.0).await;
    assert!(released, "pending SSE source leaked after peer disconnect");
    assert_eq!(before_shutdown, 1, "release must precede server shutdown");
}
