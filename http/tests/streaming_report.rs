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
    mode: u8,
}
#[async_trait::async_trait]
impl PersistenceStore for Store {
    async fn append(&self, event: PersistenceEnvelope) -> anyhow::Result<()> {
        self.backing.append(event).await
    }
    async fn load(&self, trace: &str) -> anyhow::Result<Option<PersistedTrace>> {
        if self.mode == 1 {
            anyhow::bail!("private-history-error");
        }
        self.backing.load(trace).await
    }
    async fn resume(&self, trace: &str, step: u64) -> anyhow::Result<ResumeCursor> {
        self.backing.resume(trace, step).await
    }
    async fn complete(&self, trace: &str, state: CompletionState) -> anyhow::Result<()> {
        self.backing.complete(trace, state).await?;
        if self.mode == 2 {
            anyhow::bail!("private-completion-error");
        }
        Ok(())
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
struct Fault;
#[async_trait::async_trait]
impl Transition<String, String> for Fault {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: String, _: &(), _: &mut Bus) -> Outcome<String, String> {
        Outcome::Fault("private-business-error".into())
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
        if self.1 == 5 {
            bus.cancellation_token()
                .unwrap()
                .cancel(ranvier_core::CancellationReason::Explicit);
            return std::future::pending().await;
        }
        if self.1 == 4 {
            return Err("private-init-error".into());
        }
        Ok(Box::pin(futures_util::stream::iter(vec![
            "public-item".into(),
        ])))
    }
}
#[tokio::test]
async fn prefix_durability_and_cleanup_precede_sse_success_without_private_results() {
    for mode in 0..6 {
        let effects = Arc::new(AtomicUsize::new(0));
        let compensations = Arc::new(AtomicUsize::new(0));
        let initialized = Arc::new(AtomicUsize::new(0));
        let backing = InMemoryPersistenceStore::new();
        let store: Arc<dyn PersistenceStore> = Arc::new(Store {
            backing: backing.clone(),
            mode,
        });
        let base = Axon::<(), (), String>::start("http-prefix-report");
        let prefix = if mode == 3 {
            base.with_saga_policy(SagaPolicy::Enabled)
                .then_compensated(Effect(effects.clone()), Comp(compensations.clone()))
                .then(Fault)
        } else {
            base.then(Effect(effects.clone()))
        };
        let app = TestApp::new(
            Ranvier::http::<()>()
                .bus_injector(move |_, bus| {
                    bus.insert(PersistenceHandle::from_arc(store.clone()));
                    bus.insert(PersistenceTraceId::new("sse-report"));
                })
                .post_sse(
                    "/stream",
                    prefix.then_stream(Init(initialized.clone(), mode)),
                ),
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
            [200, 503, 409, 409, 500, 503][usize::from(mode)]
        );
        let body = response.text().unwrap();
        assert!(!body.contains(PRIVATE));
        assert!(!body.contains("private-history-error"));
        assert!(!body.contains("private-completion-error"));
        assert!(!body.contains("private-compensation-error"));
        assert!(!body.contains("private-business-error"));
        assert!(!body.contains("private-init-error"));
        if mode == 0 {
            assert!(body.contains("public-item"));
            assert_eq!(body.matches("data: [DONE]").count(), 1);
            assert!(!body.contains("event: error"));
            // TestApp retains HTTP/1.1 chunk framing, so decode before
            // asserting application-level termination.
            let mut wire = response.body();
            let mut decoded = Vec::new();
            while !wire.is_empty() {
                let end = wire.windows(2).position(|v| v == b"\r\n").unwrap();
                let size =
                    usize::from_str_radix(std::str::from_utf8(&wire[..end]).unwrap(), 16).unwrap();
                wire = &wire[end + 2..];
                if size == 0 {
                    assert_eq!(wire, b"\r\n");
                    break;
                }
                decoded.extend_from_slice(&wire[..size]);
                assert_eq!(&wire[size..size + 2], b"\r\n");
                wire = &wire[size + 2..];
            }
            assert!(decoded.ends_with(b"data: [DONE]\n\n"));
            assert_eq!(initialized.load(Ordering::SeqCst), 1);
        } else {
            assert_eq!(response.header("cache-control").unwrap(), "no-store");
            assert!(!body.contains("data:"));
            let report: serde_json::Value = serde_json::from_str(body).unwrap();
            assert!(!report.get("result").is_some_and(|result| !result.is_null()));
            assert_eq!(initialized.load(Ordering::SeqCst), usize::from(mode >= 4));
            if mode == 2 {
                assert_eq!(report["operation"], "complete");
                assert!(
                    backing
                        .load("sse-report")
                        .await
                        .unwrap()
                        .unwrap()
                        .completion
                        .is_some()
                );
            }
            if mode >= 4 {
                assert_eq!(report["prefix"]["durability"], "recorded");
                assert_eq!(report["prefix"]["completion"], "confirmed");
                assert_eq!(report["initialization"], "may_have_run");
                assert_eq!(report["reconciliation_required"], true);
                if mode == 5 {
                    assert_eq!(report["reason"], "explicit");
                }
                // Stored success acknowledges the prefix only. The failed initializer
                // still returns500: this trace is not proof of streaming completion.
                assert_eq!(
                    backing
                        .load("sse-report")
                        .await
                        .unwrap()
                        .unwrap()
                        .completion,
                    Some(CompletionState::Success)
                );
            }
            if mode == 3 {
                assert_eq!(report["cleanup"]["steps"][0]["outcome"], "fault");
            }
        }
        assert_eq!(effects.load(Ordering::SeqCst), usize::from(mode != 1));
        assert_eq!(compensations.load(Ordering::SeqCst), usize::from(mode == 3));
    }
}
