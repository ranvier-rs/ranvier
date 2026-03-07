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
//! Creates `./audit-demo-output/audit.jsonl` with HMAC-signed event records.

use async_trait::async_trait;
use ranvier_audit::file_sink::FileAuditSink;
use ranvier_audit::{AuditEvent, AuditLogger};
use ranvier_core::bus::Bus;
use ranvier_core::outcome::Outcome;
use ranvier_core::transition::Transition;
use ranvier_runtime::Axon;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

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
        let logger = bus.read::<SharedAuditLogger>().cloned();

        // Create a 5W audit event
        // WHO:   input.user_id
        // WHAT:  input.action
        // WHERE: input.resource
        // WHEN:  auto-timestamped by AuditEvent::new
        // WHY:   input.detail (as intent)
        let event = AuditEvent::new(
            format!("evt-{}", timestamp_id()),
            input.user_id.clone(),
            input.action.clone(),
            input.resource.clone(),
        )
        .with_intent(&input.detail)
        .with_metadata("source", "audit-demo")
        .with_metadata("version", "0.19.0");

        // Log the event if logger is available
        if let Some(logger) = logger {
            if let Err(e) = logger.log(event).await {
                return Outcome::Fault(format!("Audit logging failed: {e}"));
            }
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

fn timestamp_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{ts:020}")
}

// ── Main ──────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let output_dir = "./audit-demo-output";
    let audit_file = format!("{output_dir}/audit.jsonl");

    println!("Audit Demo");
    println!("==========");
    println!("  Output: {audit_file}");
    println!();

    // Initialize FileAuditSink with HMAC signing key
    let sink = FileAuditSink::new(&audit_file, b"demo-signing-key-change-in-production").await?;
    let logger: SharedAuditLogger = Arc::new(AuditLogger::new(sink));

    // Build the audited pipeline
    let axon = Axon::<UserAction, UserAction, String>::new("AuditedPipeline")
        .then(AuditedAction);

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
        let desc = format!("{} {} {}", action.user_id, action.action, action.resource);
        // Inject AuditLogger into a fresh Bus for each execution
        let mut bus = Bus::new();
        bus.insert(logger.clone());

        let result = axon.execute(action, &(), &mut bus).await;
        match result {
            Outcome::Next(r) => println!("  [OK] {desc} -> {}", r.message),
            Outcome::Fault(e) => println!("  [ERR] {desc} -> {e}"),
            _ => {}
        }
    }

    // Read back and display the audit log
    println!();
    println!("Audit log contents ({audit_file}):");
    println!("---");
    let contents = tokio::fs::read_to_string(&audit_file).await?;
    for (i, line) in contents.lines().enumerate() {
        let parsed: serde_json::Value = serde_json::from_str(line)?;
        let event = &parsed["event"];
        println!(
            "  [{}] {} | {} {} -> {} | sig: {}...",
            i + 1,
            event["timestamp"].as_str().unwrap_or("?"),
            event["actor"].as_str().unwrap_or("?"),
            event["action"].as_str().unwrap_or("?"),
            event["target"].as_str().unwrap_or("?"),
            &parsed["signature"].as_str().unwrap_or("?")[..16],
        );
    }
    println!("---");
    println!("Each record is HMAC-SHA256 signed for tamper evidence.");

    // Cleanup
    let _ = tokio::fs::remove_dir_all(output_dir).await;

    Ok(())
}
