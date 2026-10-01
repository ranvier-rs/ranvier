use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper_util::rt::TokioIo;
use ranvier_core::{Bus, Outcome, Transition};
use ranvier_http::prelude::*;
use ranvier_runtime::Axon;
use ranvier_runtime::persistence::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

#[derive(Clone, Copy)]
enum Failure {
    None,
    Entry,
    Result,
    Terminal,
    Complete,
}

struct Store {
    backing: InMemoryPersistenceStore,
    failure: Failure,
    apply_unknown: bool,
    writes: AtomicUsize,
    completions: AtomicUsize,
}

#[async_trait::async_trait]
impl PersistenceStore for Store {
    async fn append(&self, event: PersistenceEnvelope) -> anyhow::Result<()> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        let unknown = match self.failure {
            Failure::None => false,
            Failure::Entry => event.outcome_kind == "Enter",
            Failure::Result => event.node_id.is_some() && event.outcome_kind == "Next",
            Failure::Terminal => event.node_id.is_none() && event.outcome_kind != "Enter",
            Failure::Complete => false,
        };
        if !unknown || self.apply_unknown {
            self.backing.append(event).await?;
        }
        if unknown {
            anyhow::bail!("private-backend-probe: rejected or committed without acknowledgement");
        }
        Ok(())
    }
    async fn load(&self, trace: &str) -> anyhow::Result<Option<PersistedTrace>> {
        self.backing.load(trace).await
    }
    async fn resume(&self, trace: &str, step: u64) -> anyhow::Result<ResumeCursor> {
        self.backing.resume(trace, step).await
    }
    async fn complete(&self, trace: &str, completion: CompletionState) -> anyhow::Result<()> {
        self.completions.fetch_add(1, Ordering::SeqCst);
        let unknown = matches!(self.failure, Failure::Complete);
        if !unknown || self.apply_unknown {
            self.backing.complete(trace, completion).await?;
        }
        if unknown {
            anyhow::bail!("private-backend-probe: completion acknowledgement unknown");
        }
        Ok(())
    }
    async fn save_intervention(
        &self,
        trace: &str,
        intervention: Intervention,
    ) -> anyhow::Result<()> {
        self.backing.save_intervention(trace, intervention).await
    }
}

#[derive(Clone)]
struct Business(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl Transition<(), u32> for Business {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: (), _: &(), _: &mut Bus) -> Outcome<u32, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::Next(7)
    }
}
#[derive(Clone)]
struct Fail;
#[async_trait::async_trait]
impl Transition<u32, u32> for Fail {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: u32, _: &(), _: &mut Bus) -> Outcome<u32, String> {
        Outcome::Fault("original-business-fault".into())
    }
}
#[derive(Clone)]
struct FirstFault(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl Transition<(), u32> for FirstFault {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: (), _: &(), _: &mut Bus) -> Outcome<u32, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::Fault("original-business-fault".into())
    }
}
#[derive(Clone)]
struct Undo(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl Transition<(), ()> for Undo {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: (), _: &(), _: &mut Bus) -> Outcome<(), String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::Fault("private-undo-probe".into())
    }
}

fn store(failure: Failure, apply_unknown: bool) -> Arc<Store> {
    Arc::new(Store {
        backing: InMemoryPersistenceStore::new(),
        failure,
        apply_unknown,
        writes: AtomicUsize::new(0),
        completions: AtomicUsize::new(0),
    })
}

struct AbortTask<T>(tokio::task::JoinHandle<T>);
impl<T> Drop for AbortTask<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn request(
    axon: Axon<(), u32, String>,
    store: Arc<Store>,
) -> (http::StatusCode, serde_json::Value) {
    request_with_store(axon, Some(store)).await
}

async fn request_with_store<T>(
    axon: Axon<(), T, String>,
    store: Option<Arc<Store>>,
) -> (http::StatusCode, serde_json::Value)
where
    T: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = probe.local_addr().unwrap();
    drop(probe);
    let ingress = HttpIngress::<()>::new()
        .bind(address.to_string())
        .bus_injector(move |_, bus| {
            if let Some(store) = &store {
                bus.insert(PersistenceHandle::from_arc(store.clone()));
                bus.insert(PersistenceTraceId::new("http-report-fixture"));
            }
        })
        .get_json_out("/execute", axon);
    let shutdown = ranvier_core::cancellation::CancellationToken::new();
    let server_token = shutdown.clone();
    let mut server = AbortTask(tokio::spawn(async move {
        ingress.run_with_cancellation((), server_token).await
    }));
    let stream = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match tokio::net::TcpStream::connect(address).await {
                Ok(stream) => break stream,
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    })
    .await
    .unwrap();
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .unwrap();
    let mut driver = AbortTask(tokio::spawn(connection));
    let response = tokio::time::timeout(
        Duration::from_secs(3),
        sender.send_request(
            http::Request::builder()
                .uri("/execute")
                .header("host", "fixture")
                .header("connection", "close")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let status = response.status();
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "application/json"
    );
    let body = tokio::time::timeout(Duration::from_secs(3), response.into_body().collect())
        .await
        .unwrap()
        .unwrap()
        .to_bytes();
    let text = std::str::from_utf8(&body).unwrap();
    assert!(!text.contains("private-backend-probe"));
    assert!(!text.contains("private-undo-probe"));
    assert!(!text.contains("private-serializer-probe"));
    let payload = serde_json::from_slice(&body).unwrap();
    drop(sender);
    let _ = shutdown.cancel(ranvier_core::cancellation::CancellationReason::Explicit);
    tokio::time::timeout(Duration::from_secs(3), &mut driver.0)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), &mut server.0)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    (status, payload)
}

#[tokio::test]
async fn entry_unknown_returns_503_without_effect_for_applied_and_unapplied_writes() {
    for applied in [false, true] {
        let store = store(Failure::Entry, applied);
        let effects = Arc::new(AtomicUsize::new(0));
        let axon = Axon::<(), (), String>::new("entry-http").then(Business(effects.clone()));
        let (status, body) = request(axon, store.clone()).await;
        assert_eq!(status, http::StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["durability"], "blocked_before_effect");
        assert_eq!(body["operation"], "intent");
        assert_eq!(body["effect"], "not_invoked");
        assert_eq!(body["reconciliation_required"], false);
        assert_eq!(effects.load(Ordering::SeqCst), 0);
        assert_eq!(store.writes.load(Ordering::SeqCst), 1);
        assert_eq!(store.completions.load(Ordering::SeqCst), 0);
        let trace = store.backing.load("http-report-fixture").await.unwrap();
        assert_eq!(trace.is_some(), applied);
    }
}

#[tokio::test]
async fn unknown_result_returns_409_with_known_domain_result_even_after_completion_ack() {
    for applied in [false, true] {
        let store = store(Failure::Result, applied);
        let effects = Arc::new(AtomicUsize::new(0));
        let axon = Axon::<(), (), String>::new("result-http").then(Business(effects.clone()));
        let (status, body) = request(axon, store.clone()).await;
        assert_eq!(status, http::StatusCode::CONFLICT);
        assert_eq!(body["durability"], "unconfirmed");
        assert_eq!(body["operation"], "result");
        assert_eq!(body["effect"], "may_have_run");
        assert_eq!(body["result"]["terminal"], "outcome");
        assert_eq!(body["result"]["value"]["Next"], 7);
        assert_eq!(body["reconciliation_required"], true);
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        assert_eq!(store.completions.load(Ordering::SeqCst), 1);
        let trace = store
            .backing
            .load("http-report-fixture")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(trace.completion, Some(CompletionState::Success));
        assert_eq!(
            trace
                .events
                .iter()
                .any(|event| event.node_id.is_some() && event.outcome_kind == "Next"),
            applied
        );
    }
}

#[tokio::test]
async fn failed_saga_cleanup_returns_409_without_replacing_the_original_fault() {
    let store = store(Failure::None, false);
    let effects = Arc::new(AtomicUsize::new(0));
    let undos = Arc::new(AtomicUsize::new(0));
    let axon = Axon::<(), (), String>::new("cleanup-http")
        .with_saga_policy(ranvier_core::saga::SagaPolicy::Enabled)
        .then_compensated(Business(effects.clone()), Undo(undos.clone()))
        .then(Fail);
    let (status, body) = request(axon, store.clone()).await;
    assert_eq!(status, http::StatusCode::CONFLICT);
    assert_eq!(body["durability"], "recorded");
    assert_eq!(body["result"]["terminal"], "outcome");
    assert_eq!(body["result"]["value"]["Fault"], "original-business-fault");
    assert_eq!(body["cleanup"]["finished"], true);
    assert_eq!(body["cleanup"]["steps"][0]["outcome"], "fault");
    assert_eq!(body["reconciliation_required"], true);
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(undos.load(Ordering::SeqCst), 1);
    let trace = store
        .backing
        .load("http-report-fixture")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(trace.completion, Some(CompletionState::Fault));
}

#[tokio::test]
async fn later_terminal_and_completion_unknowns_never_become_http_success() {
    for (failure, operation) in [
        (Failure::Terminal, "terminal"),
        (Failure::Complete, "complete"),
    ] {
        for applied in [false, true] {
            let store = store(failure, applied);
            let effects = Arc::new(AtomicUsize::new(0));
            let axon = Axon::<(), (), String>::new("late-http").then(Business(effects.clone()));
            let (status, body) = request(axon, store.clone()).await;
            assert_eq!(status, http::StatusCode::CONFLICT);
            assert_eq!(body["operation"], operation);
            assert_eq!(body["durability"], "unconfirmed");
            assert_eq!(body["effect"], "may_have_run");
            assert_eq!(body["result"]["terminal"], "outcome");
            assert_eq!(body["result"]["value"]["Next"], 7);
            assert_eq!(body["reconciliation_required"], true);
            assert_eq!(effects.load(Ordering::SeqCst), 1);
            let trace = store
                .backing
                .load("http-report-fixture")
                .await
                .unwrap()
                .unwrap();
            if operation == "terminal" {
                assert_eq!(
                    trace
                        .events
                        .iter()
                        .any(|event| event.node_id.is_none() && event.outcome_kind != "Enter"),
                    applied
                );
                assert_eq!(trace.completion, Some(CompletionState::Success));
            } else {
                assert_eq!(
                    trace.completion,
                    applied.then_some(CompletionState::Success)
                );
            }
        }
    }
}

#[tokio::test]
async fn direct_failed_compensation_is_owned_by_the_http_report() {
    let store = store(Failure::None, false);
    let effects = Arc::new(AtomicUsize::new(0));
    let undos = Arc::new(AtomicUsize::new(0));
    let axon = Axon::<(), (), String>::new("direct-cleanup-http")
        .with_saga_policy(ranvier_core::saga::SagaPolicy::Disabled)
        .then_compensated(FirstFault(effects.clone()), Undo(undos.clone()));
    let (status, body) = request(axon, store).await;
    assert_eq!(status, http::StatusCode::CONFLICT);
    assert_eq!(body["durability"], "recorded");
    assert_eq!(body["result"]["value"]["Fault"], "original-business-fault");
    assert_eq!(body["cleanup"]["finished"], true);
    assert_eq!(body["cleanup"]["steps"].as_array().unwrap().len(), 1);
    assert_eq!(body["cleanup"]["steps"][0]["outcome"], "fault");
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(undos.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn fully_acknowledged_execution_keeps_normal_json_success() {
    let store = store(Failure::None, false);
    let effects = Arc::new(AtomicUsize::new(0));
    let axon = Axon::<(), (), String>::new("confirmed-http").then(Business(effects.clone()));
    let (status, body) = request(axon, store.clone()).await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(body, 7);
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(store.completions.load(Ordering::SeqCst), 1);
}

#[derive(serde::Deserialize)]
struct BrokenOutput;

#[tokio::test]
async fn legacy_null_trace_is_blocked_over_tcp_without_effect_or_store_mutation() {
    let store = store(Failure::None, false);
    let effects = Arc::new(AtomicUsize::new(0));
    let axon = Axon::<(), (), String>::new("legacy-null-http").then(Business(effects.clone()));
    let schematic = axon.schematic();
    store
        .backing
        .append(PersistenceEnvelope {
            trace_id: "http-report-fixture".into(),
            circuit: schematic.name.clone(),
            schematic_version: schematic.schema_version.clone(),
            step: 1,
            node_id: Some(schematic.nodes[1].id.to_string()),
            outcome_kind: "Next".into(),
            timestamp_ms: 0,
            payload_hash: None,
            payload: Some(serde_json::Value::Null),
        })
        .await
        .unwrap();
    let before =
        serde_json::to_value(store.backing.load("http-report-fixture").await.unwrap()).unwrap();
    let (status, body) = request(axon, store.clone()).await;
    assert_eq!(status, http::StatusCode::CONFLICT);
    assert_eq!(body["durability"], "blocked_before_effect");
    assert_eq!(body["operation"], "recovery_payload");
    assert_eq!(body["reconciliation_required"], true);
    assert_eq!(body["effect"], "not_invoked");
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert_eq!(store.writes.load(Ordering::SeqCst), 0);
    assert_eq!(store.completions.load(Ordering::SeqCst), 0);
    assert_eq!(
        serde_json::to_value(store.backing.load("http-report-fixture").await.unwrap()).unwrap(),
        before
    );
}
impl serde::Serialize for BrokenOutput {
    fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom("private-serializer-probe"))
    }
}

#[derive(Clone)]
struct BrokenBusiness(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl Transition<(), BrokenOutput> for BrokenBusiness {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: (), _: &(), _: &mut Bus) -> Outcome<BrokenOutput, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::Next(BrokenOutput)
    }
}

#[tokio::test]
async fn output_encoding_failure_over_tcp_does_not_prove_recoverable_completion() {
    for persisted in [false, true] {
        let store = store(Failure::None, false);
        let effects = Arc::new(AtomicUsize::new(0));
        let axon =
            Axon::<(), (), String>::new("broken-output-http").then(BrokenBusiness(effects.clone()));
        let (status, body) = request_with_store(axon, persisted.then(|| store.clone())).await;
        assert_eq!(status, http::StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            body["error"],
            if persisted {
                "Execution report could not be encoded"
            } else {
                "JSON response could not be encoded"
            }
        );
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        assert_eq!(store.completions.load(Ordering::SeqCst), 0);
        if persisted {
            let trace = store
                .backing
                .load("http-report-fixture")
                .await
                .unwrap()
                .unwrap();
            assert_eq!(trace.completion, None);
            assert!(
                trace
                    .events
                    .iter()
                    .all(|event| event.outcome_kind != "Next")
            );
            assert_eq!(body["reconciliation_required"], true);
        } else {
            assert_eq!(store.writes.load(Ordering::SeqCst), 0);
            assert!(
                store
                    .backing
                    .load("http-report-fixture")
                    .await
                    .unwrap()
                    .is_none()
            );
        }
    }
}
