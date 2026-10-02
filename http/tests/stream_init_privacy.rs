#![cfg(feature = "streaming")]
use ranvier_core::{Bus, streaming::StreamingTransition};
use ranvier_http::prelude::{Ranvier, TestApp, TestRequest};
use ranvier_runtime::Axon;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
#[derive(Clone)]
struct Fail {
    init: bool,
}
struct PrivateItem;
static ENCODE_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
impl serde::Serialize for PrivateItem {
    fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
        ENCODE_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(serde::ser::Error::custom("private-item-encode-marker"))
    }
}
#[async_trait::async_trait]
impl StreamingTransition<()> for Fail {
    type Item = PrivateItem;
    type Error = String;
    type Resources = ();
    async fn run_stream(
        &self,
        _: (),
        _: &(),
        _: &mut Bus,
    ) -> Result<Pin<Box<dyn futures_util::Stream<Item = PrivateItem> + Send>>, String> {
        if self.init {
            Err("private-stream-init-marker".into())
        } else {
            Ok(Box::pin(futures_util::stream::iter(vec![
                PrivateItem,
                PrivateItem,
            ])))
        }
    }
}
struct Capture {
    logs: Arc<Mutex<String>>,
    spans: std::sync::atomic::AtomicU64,
}
impl tracing::Subscriber for Capture {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(
            self.spans
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                + 1,
        )
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct Visit<'a>(&'a mut String);
        impl tracing::field::Visit for Visit<'_> {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                use std::fmt::Write;
                write!(self.0, "{}={value:?} ", field.name()).unwrap();
            }
        }
        event.record(&mut Visit(&mut self.logs.lock().unwrap()));
    }
}
#[tokio::test]
async fn streaming_init_failure_redacts_response_and_actual_runtime_logs() {
    const ISOLATED: &str = "RANVIER_STREAM_PRIVACY_ISOLATED";
    if std::env::var_os(ISOLATED).is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "streaming_init_failure_redacts_response_and_actual_runtime_logs",
                "--test-threads=1",
            ])
            .env(ISOLATED, "1")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "isolated capture failed: {} {}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(String::from_utf8_lossy(&result.stdout).contains("1 passed; 0 failed"));
        return;
    }
    let logs = Arc::new(Mutex::new(String::new()));
    tracing::subscriber::set_global_default(Capture {
        logs: logs.clone(),
        spans: std::sync::atomic::AtomicU64::new(0),
    })
    .unwrap();
    let stream = Axon::<(), (), String>::start("private-init").then_stream(Fail { init: true });
    let app = TestApp::new(Ranvier::http::<()>().post_sse("/stream", stream), ());
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        app.send(TestRequest::post("/stream")),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(response.status(), 500);
    let logs_handle = logs.clone();
    let logs = logs.lock().unwrap().clone();
    assert!(
        logs.contains("Streaming"),
        "capture must contain an operational failure event"
    );
    assert!(
        !logs.contains("private-stream-init-marker"),
        "stream initializer leaked into logs"
    );
    let body: serde_json::Value = response.json().unwrap();
    assert!(!body.to_string().contains("private-stream-init-marker"));
    assert_eq!(body["error"], "Streaming initialization failed");
    let stream = Axon::<(), (), String>::start("private-item").then_stream(Fail { init: false });
    let app = TestApp::new(Ranvier::http::<()>().post_sse("/stream", stream), ());
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        app.send(TestRequest::post("/stream")),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        ENCODE_CALLS.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "producer must stop encoding after its first failure"
    );
    assert!(response.text().unwrap().contains("serialize_error"));
    assert_eq!(response.text().unwrap().matches("event: error").count(), 1);
    assert!(
        !response.text().unwrap().contains("[DONE]"),
        "encoding failure must not announce normal completion"
    );
    assert!(
        !response
            .text()
            .unwrap()
            .contains("private-item-encode-marker")
    );
    let logs = logs_handle.lock().unwrap().clone();
    assert!(
        !logs.contains("private-item-encode-marker"),
        "item encoder leaked into logs"
    );
    assert!(
        logs.contains("stream_item_encode"),
        "late failure operational event must be captured"
    );
}
