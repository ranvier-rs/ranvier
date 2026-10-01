//! # Audit Demo
//!
//! Demonstrates Ranvier's audit logging infrastructure with tamper-evident
//! file-based event recording and Bus-injected audit logging.
//!
//! ## Run
//! ```bash
//! cargo run -p audit-demo
//! ```
//!
//! ## Key APIs
//! - `AuditLogger<S>` — generic logger parameterized by sink
//! - `AuditEvent` — 5W audit payload (Who, What, Where, When, Why)
//! - `FileAuditSink` — HMAC-SHA256 signed JSONL file sink
//! - `PostgresAuditSink` — PostgreSQL sink with hash chain integrity (feature = "postgres")
//! - `AuditSink` trait — implement for custom sinks (database, queue, etc.)
//!
//! ## Output
//! Creates an exclusively owned temporary directory with signed event records.

use async_trait::async_trait;
use ranvier_audit::file_sink::FileAuditSink;
use ranvier_audit::{AuditEvent, AuditLogger};
use ranvier_core::bus::Bus;
use ranvier_core::outcome::Outcome;
use ranvier_core::transition::Transition;
use ranvier_runtime::Axon;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

type DemoError = Box<dyn std::error::Error + Send + Sync>;
fn local_demo_outcome<T, E>(
    report: ranvier_runtime::ExecutionReport<Outcome<T, E>>,
) -> Result<Outcome<T, E>, DemoError> {
    if report
        .cleanup()
        .is_some_and(|cleanup| !cleanup.is_complete())
    {
        return Err("Execution requires reconciliation before retry".into());
    }
    match report {
        ranvier_runtime::ExecutionReport::Unpersisted { result, .. } => Ok(result),
        _ => Err("Execution requires reconciliation before retry".into()),
    }
}

// ── Domain types ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UserAction {
    user_id: String,
    action: String,
    resource: String,
    detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ActionResult {
    success: bool,
    message: String,
}

// ── Shared logger type alias ──────────────────────────────────────────────

/// Wrap AuditLogger in Arc for Bus injection (Bus requires Clone-free insert).
type SharedAuditLogger = Arc<AuditLogger<FileAuditSink>>;

// ── Audited Transition ────────────────────────────────────────────────────

/// A transition that logs audit events through Bus-injected AuditLogger.
///
/// The AuditLogger is retrieved from the Bus at runtime, demonstrating
/// the resource injection pattern used in production Ranvier pipelines.
#[derive(Clone)]
struct AuditedAction;

#[async_trait]
impl Transition<UserAction, ActionResult> for AuditedAction {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: UserAction,
        _resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<ActionResult, String> {
        // Retrieve the AuditLogger from Bus (injected via Arc wrapper)
        let Ok(logger) = bus.get_cloned::<SharedAuditLogger>() else {
            return Outcome::Fault("Required audit capability missing".into());
        };

        // Create a 5W audit event
        // WHO:   input.user_id
        // WHAT:  input.action
        // WHERE: input.resource
        // WHEN:  auto-timestamped by AuditEvent::new
        // WHY:   input.detail (as intent)
        let event = AuditEvent::new(
            format!(
                "evt-{}-{}",
                std::process::id(),
                EVENT_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ),
            input.user_id.clone(),
            input.action.clone(),
            input.resource.clone(),
        )
        .with_intent(&input.detail)
        .with_metadata("source", "audit-demo")
        .with_metadata("version", env!("CARGO_PKG_VERSION"));

        if logger.log(event).await.is_err() {
            return Outcome::Fault("Audit logging failed; reconcile before retry".into());
        }

        Outcome::Next(ActionResult {
            success: true,
            message: format!(
                "Action '{}' on '{}' by '{}' completed",
                input.action, input.resource, input.user_id
            ),
        })
    }
}

// Process-local fixture identity only; not a persisted global event contract.
static EVENT_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

// ── Main ──────────────────────────────────────────────────────────────────

async fn run_demo() -> Result<(), DemoError> {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let output_dir =
        std::env::temp_dir().join(format!("ranvier-audit-{}-{nonce}", std::process::id()));
    tokio::fs::create_dir(&output_dir).await?;
    let audit_file = output_dir.join("audit.jsonl");

    println!("Audit Demo");
    println!("==========");
    println!("  Output: {audit_file:?}");
    println!();

    // Initialize FileAuditSink with HMAC signing key
    let sink = FileAuditSink::new(&audit_file, b"demo-signing-key-change-in-production").await?;
    let logger: SharedAuditLogger = Arc::new(AuditLogger::new(sink));

    // Build the audited pipeline
    let axon = Axon::<UserAction, UserAction, String>::new("AuditedPipeline").then(AuditedAction);

    // Simulate various user actions
    let actions = vec![
        UserAction {
            user_id: "admin@example.com".into(),
            action: "CREATE".into(),
            resource: "user:456".into(),
            detail: "New team member onboarding".into(),
        },
        UserAction {
            user_id: "admin@example.com".into(),
            action: "UPDATE".into(),
            resource: "user:456/role".into(),
            detail: "Role change: viewer -> editor".into(),
        },
        UserAction {
            user_id: "security@example.com".into(),
            action: "DELETE".into(),
            resource: "api-key:ak-789".into(),
            detail: "Compromised key rotation".into(),
        },
        UserAction {
            user_id: "system".into(),
            action: "TRANSITION".into(),
            resource: "order:ORD-100/status".into(),
            detail: "State: pending -> processing".into(),
        },
    ];

    for action in actions {
        // Inject AuditLogger into a fresh Bus for each execution
        let mut bus = Bus::new();
        bus.insert(logger.clone());

        let result = local_demo_outcome(axon.execute(action, &(), &mut bus).await)?;
        if !matches!(result, Outcome::Next(ref value) if value.success) {
            return Err("Audit scenario did not succeed; reconcile before retry".into());
        }
    }

    // Read back and display the audit log
    println!();
    println!("Audit log contents ({audit_file:?}):");
    println!("---");
    let contents = tokio::fs::read_to_string(&audit_file).await?;
    let records: Vec<serde_json::Value> = contents
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    if records.len() != 4
        || records.iter().any(|record| {
            record["signature"]
                .as_str()
                .is_none_or(|sig| sig.len() != 64)
        })
    {
        return Err("Unexpected audit file records".into());
    }
    println!("Four signed records observed (signature shape only; no verification claimed)");
    drop(logger);
    tokio::fs::remove_file(&audit_file).await?;
    tokio::fs::remove_dir(&output_dir).await?;

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), DemoError> {
    run_demo().await
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn actual_file_sink_and_scoped_cleanup() {
        run_demo().await.unwrap();
    }
    #[tokio::test]
    async fn missing_logger_rejects_before_success() {
        let result = AuditedAction
            .run(
                UserAction {
                    user_id: "private".into(),
                    action: "CREATE".into(),
                    resource: "private".into(),
                    detail: "private".into(),
                },
                &(),
                &mut Bus::new(),
            )
            .await;
        assert!(
            matches!(result, Outcome::Fault(error) if error == "Required audit capability missing")
        );
    }
}
