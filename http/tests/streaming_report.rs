#![cfg(feature = "streaming")]
use ranvier_core::{Bus, Outcome, Transition, saga::SagaPolicy, streaming::StreamingTransition};
use ranvier_http::prelude::{Ranvier, TestApp, TestRequest};
use ranvier_runtime::{Axon, persistence::*};
use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
const PRIVATE: &str = "private-prefix-value-should-not-enter-SSE-rejection";
struct Store {
    backing: InMemoryPersistenceStore,
    fail_load: bool,
}
#[async_trait::async_trait]
impl PersistenceStore for Store {
    async fn append(&self, event: PersistenceEnvelope) -> anyhow::Result<()> {
        self.backing.append(event).await
    }
    async fn load(&self, trace: &str) -> anyhow::Result<Option<PersistedTrace>> {
        if self.fail_load {
            anyhow::bail!("private-history-error");
        }
        self.backing.load(trace).await
    }
    async fn resume(&self, trace: &str, step: u64) -> anyhow::Result<ResumeCursor> {
        self.backing.resume(trace, step).await
    }
    async fn complete(&self, trace: &str, state: CompletionState) -> anyhow::Result<()> {
        self.backing.complete(trace, state).await
    }
    async fn save_intervention(&self, trace: &str, event: Intervention) -> anyhow::Result<()> {
        self.backing.save_intervention(trace, event).await
    }
}
#[derive(Clone)]
struct Effect(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl Transition<(), String> for Effect {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: (), _: &(), _: &mut Bus) -> Outcome<String, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::Next(PRIVATE.into())
    }
}
#[derive(Clone)]
struct Comp(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl Transition<(), ()> for Comp {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: (), _: &(), _: &mut Bus) -> Outcome<(), String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::Fault("private-compensation-error".into())
    }
}
#[derive(Clone)]
struct CompSuccess(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl Transition<(), ()> for CompSuccess {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: (), _: &(), _: &mut Bus) -> Outcome<(), String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::Next(())
    }
}
#[derive(Clone)]
struct Init(Arc<AtomicUsize>, u8);
#[async_trait::async_trait]
impl StreamingTransition<String> for Init {
    type Item = String;
    type Error = String;
    type Resources = ();
    async fn run_stream(
        &self,
        _: String,
        _: &(),
        bus: &mut Bus,
    ) -> Result<Pin<Box<dyn futures_util::Stream<Item = String> + Send>>, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        if self.1 == 8 {
            panic!("private-init-panic");
        }
        if self.1 == 5 {
            bus.cancellation_token()
                .unwrap()
                .cancel(ranvier_core::CancellationReason::Explicit);
            return std::future::pending().await;
        }
        if self.1 == 4 {
            return Err("private-init-error".into());
        }
        if self.1 == 6 {
            use futures_util::StreamExt;
            return Ok(Box::pin(
                futures_util::stream::iter(vec!["public-item".into()])
                    .chain(futures_util::stream::pending()),
            ));
        }
        if self.1 == 7 {
            let token = bus.cancellation_token().unwrap().clone();
            let mut first = true;
            return Ok(Box::pin(futures_util::stream::poll_fn(move |_| {
                if first {
                    first = false;
                    return std::task::Poll::Ready(Some("public-item".into()));
                }
                token.cancel(ranvier_core::CancellationReason::Explicit);
                std::task::Poll::Pending
            })));
        }
        Ok(Box::pin(futures_util::stream::iter(vec![
            "public-item".into(),
        ])))
    }
}
#[tokio::test]
async fn configured_persistence_blocks_unjournaled_stream_initialization() {
    for fail_load in [false, true] {
        let effects = Arc::new(AtomicUsize::new(0));
        let compensations = Arc::new(AtomicUsize::new(0));
        let initialized = Arc::new(AtomicUsize::new(0));
        let backing = InMemoryPersistenceStore::new();
        let store: Arc<dyn PersistenceStore> = Arc::new(Store {
            backing: backing.clone(),
            fail_load,
        });
        let prefix =
            Axon::<(), (), String>::start("http-prefix-report").then(Effect(effects.clone()));
        let app = TestApp::new(
            Ranvier::http::<()>()
                .bus_injector(move |_, bus| {
                    bus.insert(PersistenceHandle::from_arc(store.clone()));
                    bus.insert(PersistenceTraceId::new("sse-report"));
                })
                .post_sse("/stream", prefix.then_stream(Init(initialized.clone(), 0))),
            (),
        );
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            app.send(TestRequest::post("/stream")),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            response.status().as_u16(),
            if fail_load { 503 } else { 409 },
            "fail_load={fail_load}"
        );
        let body = response.text().unwrap();
        assert!(!body.contains(PRIVATE));
        assert!(!body.contains("private-history-error"));
        assert!(!body.contains("private-completion-error"));
        assert!(!body.contains("private-compensation-error"));
        assert!(!body.contains("private-init-error"));
        assert!(!body.contains("private-init-panic"));
        assert_eq!(response.header("cache-control").unwrap(), "no-store");
        assert!(!body.contains("data:"));
        let report: serde_json::Value = serde_json::from_str(body).unwrap();
        assert!(report.get("result").is_none_or(serde_json::Value::is_null));
        assert_eq!(initialized.load(Ordering::SeqCst), 0);
        assert_eq!(effects.load(Ordering::SeqCst), usize::from(!fail_load));
        assert_eq!(compensations.load(Ordering::SeqCst), 0);
        assert_eq!(report["reconciliation_required"], !fail_load);
        if fail_load {
            assert_eq!(report["durability"], "blocked_before_effect");
            assert_eq!(report["operation"], "load");
            assert_eq!(report["effect"], "not_invoked");
        } else {
            assert_eq!(report["durability"], "unconfirmed");
            assert_eq!(report["operation"], "unsupported_effect");
            assert_eq!(report["effect"], "may_have_run");
            let trace = backing.load("sse-report").await.unwrap().unwrap();
            assert_eq!(trace.completion, None);
            assert_eq!(trace.events.last().unwrap().outcome_kind, "Next");
        }
    }
}

#[tokio::test]
async fn late_timeout_and_cancellation_emit_error_without_done() {
    for mode in [6, 7] {
        let initialized = Arc::new(AtomicUsize::new(0));
        let stream = Axon::<(), (), String>::start("late-terminal")
            .then(Effect(Arc::new(AtomicUsize::new(0))))
            .then_stream(Init(initialized.clone(), mode))
            .with_timeout(ranvier_core::streaming::StreamTimeoutConfig {
                init: None,
                idle: Some(std::time::Duration::from_millis(20)),
                total: None,
            });
        let app = TestApp::new(Ranvier::http::<()>().post_sse("/stream", stream), ());
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            app.send(TestRequest::post("/stream")),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(response.status(), 200);
        let body = response.text().unwrap();
        assert!(body.contains("public-item"));
        assert_eq!(body.matches("event: error").count(), 1);
        assert!(body.contains(if mode == 6 {
            "stream_timeout"
        } else {
            "stream_cancelled"
        }));
        assert!(!body.contains("[DONE]"));
        assert_eq!(initialized.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn initializer_cleanup_is_explicit_for_unpersisted_streams_without_private_values() {
    for complete in [false, true] {
        for mode in [4, 5, 8] {
            let effects = Arc::new(AtomicUsize::new(0));
            let undo = Arc::new(AtomicUsize::new(0));
            let init = Arc::new(AtomicUsize::new(0));
            let prefix = Axon::<(), (), String>::start("incomplete-init-cleanup")
                .with_saga_policy(SagaPolicy::Enabled);
            let prefix = if complete {
                prefix.then_compensated(Effect(effects.clone()), CompSuccess(undo.clone()))
            } else {
                prefix.then_compensated(Effect(effects.clone()), Comp(undo.clone()))
            };
            let stream = prefix.then_stream(Init(init.clone(), mode));
            let app = TestApp::new(Ranvier::http::<()>().post_sse("/stream", stream), ());
            let response = tokio::time::timeout(
                std::time::Duration::from_secs(3),
                app.send(TestRequest::post("/stream")),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(
                response.status().as_u16(),
                if mode == 5 { 503 } else { 500 }
            );
            assert_eq!(response.header("cache-control").unwrap(), "no-store");
            let body = response.text().unwrap();
            assert!(!body.contains(PRIVATE));
            assert!(!body.contains("private-compensation-error"));
            assert!(!body.contains("private-init-error"));
            assert!(!body.contains("private-init-panic"));
            assert!(!body.contains("[DONE]"));
            assert!(!body.contains("data:"));
            let report: serde_json::Value = response.json().unwrap();
            assert_eq!(report["initialization_cleanup"]["finished"], true);
            assert_eq!(
                report["initialization_cleanup"]["steps"][0]["outcome"],
                if complete { "completed" } else { "fault" }
            );
            assert_eq!(report["initialization_cleanup_complete"], complete);
            assert_eq!(report["reconciliation_required"], true);
            assert_eq!(report["prefix"]["durability"], "unpersisted");
            assert_eq!(effects.load(Ordering::SeqCst), 1);
            assert_eq!(undo.load(Ordering::SeqCst), 1);
            assert_eq!(init.load(Ordering::SeqCst), 1);
        }
    }
}
