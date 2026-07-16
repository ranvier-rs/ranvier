//! Private M419-RQ10 production-shaped load and soak fixture.
//!
//! This binary belongs to `ranvier-bench`; it is not a supported example or a
//! reusable application template. `scripts/m419_load_soak_gate.mjs` owns its
//! fixed request shape and evidence contract.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use ranvier_audit::{AuditEvent, AuditSink, InMemoryAuditSink, RetentionPolicy};
use ranvier_core::cancellation::{CancellationReason, CancellationToken};
use ranvier_core::prelude::{Bus, Outcome, ResourceRequirement, Transition};
use ranvier_http::Ranvier;
use ranvier_inspector::{get_trace_registry, metrics, payload};
use ranvier_runtime::Axon;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing_subscriber::prelude::*;

const CIRCUIT: &str = "M419CanonicalDecision";
const AUDIT_MAX_COUNT: usize = 512;

#[derive(Clone)]
struct AppState {
    audit: InMemoryAuditSink,
    accepted: Arc<AtomicU64>,
    audit_expired: Arc<AtomicU64>,
    slow_started: Arc<AtomicU64>,
}

impl ResourceRequirement for AppState {}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
struct DecisionRequest {
    subject: String,
    amount: u64,
    risk: u8,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ValidatedDecision {
    subject: String,
    amount: u64,
    risk: u8,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct EvaluatedDecision {
    subject: String,
    approved: bool,
    score: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct DecisionResponse {
    id: u64,
    subject: String,
    approved: bool,
    score: u64,
}

#[derive(Clone)]
struct Validate;

#[async_trait]
impl Transition<DecisionRequest, ValidatedDecision> for Validate {
    type Error = String;
    type Resources = AppState;

    async fn run(
        &self,
        input: DecisionRequest,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<ValidatedDecision, Self::Error> {
        if input.subject.is_empty() || input.risk > 100 {
            Outcome::Fault("invalid fixed decision input".to_string())
        } else {
            Outcome::Next(ValidatedDecision {
                subject: input.subject,
                amount: input.amount,
                risk: input.risk,
            })
        }
    }
}

#[derive(Clone)]
struct Evaluate;

#[async_trait]
impl Transition<ValidatedDecision, EvaluatedDecision> for Evaluate {
    type Error = String;
    type Resources = AppState;

    async fn run(
        &self,
        input: ValidatedDecision,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<EvaluatedDecision, Self::Error> {
        let score = input
            .amount
            .saturating_div(100)
            .saturating_add(u64::from(input.risk));
        Outcome::Next(EvaluatedDecision {
            subject: input.subject,
            approved: score < 500,
            score,
        })
    }
}

#[derive(Clone)]
struct Record;

#[async_trait]
impl Transition<EvaluatedDecision, DecisionResponse> for Record {
    type Error = String;
    type Resources = AppState;

    async fn run(
        &self,
        input: EvaluatedDecision,
        resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<DecisionResponse, Self::Error> {
        let started = Instant::now();
        let id = resources.accepted.fetch_add(1, Ordering::Relaxed) + 1;
        let event = AuditEvent::new(
            format!("rq10-{id}"),
            "load-fixture".to_string(),
            "decision".to_string(),
            input.subject.clone(),
        )
        .with_metadata("approved", input.approved)
        .with_metadata("score", input.score);
        if let Err(error) = resources.audit.append(&event).await {
            return Outcome::Fault(format!("audit append failed: {error}"));
        }

        payload::record_event(payload::CapturedEvent {
            timestamp: epoch_millis(),
            event_type: "decision_recorded".to_string(),
            node_id: Some("record".to_string()),
            circuit: Some(CIRCUIT.to_string()),
            duration_ms: Some(elapsed_millis(started)),
            outcome_type: Some(if input.approved { "approved" } else { "denied" }.to_string()),
            payload_hash: None,
            payload_json: None,
        });
        Outcome::Next(DecisionResponse {
            id,
            subject: input.subject,
            approved: input.approved,
            score: input.score,
        })
    }
}

#[derive(Clone)]
struct SlowDecision;

#[async_trait]
impl Transition<(), serde_json::Value> for SlowDecision {
    type Error = String;
    type Resources = AppState;

    async fn run(
        &self,
        _input: (),
        resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<serde_json::Value, Self::Error> {
        resources.slow_started.fetch_add(1, Ordering::Relaxed);
        tokio::time::sleep(Duration::from_secs(2)).await;
        Outcome::Next(serde_json::json!({ "completed": true }))
    }
}

#[derive(Clone)]
struct Stats;

#[async_trait]
impl Transition<(), serde_json::Value> for Stats {
    type Error = String;
    type Resources = AppState;

    async fn run(
        &self,
        _input: (),
        resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<serde_json::Value, Self::Error> {
        Outcome::Next(snapshot(resources).await)
    }
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

async fn snapshot(resources: &AppState) -> serde_json::Value {
    let trace = get_trace_registry()
        .lock()
        .map(|registry| registry.stats())
        .unwrap_or_default();
    serde_json::json!({
        "schema_version": "1.0.0",
        "accepted": resources.accepted.load(Ordering::Relaxed),
        "slow_started": resources.slow_started.load(Ordering::Relaxed),
        "trace": trace,
        "events": payload::event_buffer_stats(),
        "metrics": metrics::retention_snapshot_circuit(CIRCUIT),
        "audit": {
            "current_len": resources.audit.len().await,
            "max_count": AUDIT_MAX_COUNT,
            "expired": resources.audit_expired.load(Ordering::Relaxed)
        }
    })
}

async fn retention_loop(resources: AppState, token: CancellationToken) {
    let policy = RetentionPolicy::max_count(AUDIT_MAX_COUNT);
    loop {
        tokio::select! {
            _ = token.cancelled() => break,
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                if let Ok(expired) = resources.audit.apply_retention(&policy).await {
                    resources.audit_expired.fetch_add(
                        u64::try_from(expired.len()).unwrap_or(u64::MAX),
                        Ordering::Relaxed,
                    );
                }
            }
        }
    }
}

#[cfg(unix)]
async fn wait_for_shutdown() -> std::io::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut terminate = signal(SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result,
        _ = terminate.recv() => Ok(()),
    }
}

#[cfg(not(unix))]
async fn wait_for_shutdown() -> std::io::Result<()> {
    tokio::signal::ctrl_c().await
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::registry()
        .with(ranvier_inspector::layer())
        .try_init()?;

    let bind = std::env::var("RANVIER_RQ10_BIND").unwrap_or_else(|_| "127.0.0.1:3160".to_string());
    let summary_path = std::env::var("RANVIER_RQ10_SERVER_SUMMARY")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("target/m419-load-soak/server-summary.json"));
    let resources = AppState {
        audit: InMemoryAuditSink::new(),
        accepted: Arc::new(AtomicU64::new(0)),
        audit_expired: Arc::new(AtomicU64::new(0)),
        slow_started: Arc::new(AtomicU64::new(0)),
    };
    let token = CancellationToken::new();

    let retention = tokio::spawn(retention_loop(resources.clone(), token.clone()));
    let signal_token = token.clone();
    let signal = tokio::spawn(async move {
        if wait_for_shutdown().await.is_ok() {
            signal_token.cancel(CancellationReason::OperatorShutdown);
        }
    });

    let decision = Axon::<DecisionRequest, DecisionRequest, String, AppState>::new(CIRCUIT)
        .then(Validate)
        .then(Evaluate)
        .then(Record);
    let slow = Axon::<(), (), String, AppState>::new("M419SlowDecision").then(SlowDecision);
    let stats = Axon::<(), (), String, AppState>::new("M419Stats").then(Stats);

    println!("M419_RQ10_READY http://{bind}");
    let result = Ranvier::http::<AppState>()
        .bind(&bind)
        .graceful_shutdown(Duration::from_secs(3))
        .readiness_liveness_default()
        .post_typed_json_out("/decision", decision)
        .get_json_out("/slow", slow)
        .get_json_out("/stats", stats)
        .run_with_cancellation(resources.clone(), token.clone())
        .await;

    token.cancel(CancellationReason::OperatorShutdown);
    signal.abort();
    let _ = retention.await;
    if let Ok(expired) = resources
        .audit
        .apply_retention(&RetentionPolicy::max_count(AUDIT_MAX_COUNT))
        .await
    {
        resources.audit_expired.fetch_add(
            u64::try_from(expired.len()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }
    let summary = snapshot(&resources).await;
    if let Some(parent) = summary_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&summary_path, serde_json::to_vec_pretty(&summary)?)?;
    println!("M419_RQ10_SUMMARY {}", summary_path.display());
    result
}
