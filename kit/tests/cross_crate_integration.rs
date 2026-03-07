//! Cross-crate integration tests for M240-M242 deliverables.
//!
//! These tests verify that features from different crates work correctly when
//! combined in realistic pipelines. Each test crosses at least two crate
//! boundaries to prove integration integrity.

use async_trait::async_trait;
use ranvier::prelude::*;
use ranvier_audit::{AuditChain, AuditEvent, AuditLogger, InMemoryAuditSink};
use ranvier_compliance::Sensitive;
use ranvier_core::bus::Bus;
use ranvier_core::config::RanvierConfig;
use ranvier_core::outcome::Outcome;
use ranvier_core::transition::Transition;
use ranvier_runtime::Axon;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// ── Test domain types ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UserRequest {
    user_id: String,
    email: String,
    action: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AuditedResult {
    success: bool,
    message: String,
}

// ── Test transitions ───────────────────────────────────────────────────────

/// Transition that logs audit events using Bus-injected AuditLogger.
/// Crosses: ranvier-runtime × ranvier-audit × ranvier-core (Bus)
#[derive(Clone)]
struct AuditingTransition;

#[async_trait]
impl Transition<UserRequest, AuditedResult> for AuditingTransition {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: UserRequest,
        _resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<AuditedResult, String> {
        let logger = bus
            .read::<Arc<AuditLogger<InMemoryAuditSink>>>()
            .cloned();
        if let Some(logger) = logger {
            let event = AuditEvent::new(
                format!("evt-{}", input.user_id),
                input.user_id.clone(),
                input.action.clone(),
                "test-resource".into(),
            );
            if let Err(e) = logger.log(event).await {
                return Outcome::Fault(format!("Audit failed: {e}"));
            }
        }
        Outcome::Next(AuditedResult {
            success: true,
            message: format!("{} completed", input.action),
        })
    }
}

/// Transition that reads AccessLogRequest from Bus and writes AccessLogEntry.
/// Crosses: ranvier-runtime × ranvier-std × ranvier-core (Bus)
#[derive(Clone)]
struct AccessLogVerifier;

#[async_trait]
impl Transition<UserRequest, UserRequest> for AccessLogVerifier {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: UserRequest,
        _resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<UserRequest, String> {
        // Verify AccessLogEntry was written by AccessLogGuard
        let has_entry = bus.read::<AccessLogEntry>().is_some();
        if has_entry {
            bus.insert("access_log_verified".to_string());
        }
        Outcome::Next(input)
    }
}

// ── Integration tests ──────────────────────────────────────────────────────

/// Test: AuditLogger (ranvier-audit) integrated with Axon pipeline (ranvier-runtime)
/// via Bus injection (ranvier-core).
///
/// Crosses: audit × runtime × core
#[tokio::test]
async fn test_audit_pipeline_integration() {
    let sink = InMemoryAuditSink::new();
    let logger = Arc::new(AuditLogger::new(sink.clone()));

    let axon = Axon::<UserRequest, UserRequest, String>::new("AuditPipeline")
        .then(AuditingTransition);

    let mut bus = Bus::new();
    bus.insert(logger.clone());

    let input = UserRequest {
        user_id: "user-1".into(),
        email: "test@example.com".into(),
        action: "CREATE".into(),
    };

    let result = axon.execute(input, &(), &mut bus).await;
    assert!(matches!(result, Outcome::Next(_)));

    // Verify audit event was recorded
    let events = sink.get_events().await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].actor, "user-1");
    assert_eq!(events[0].action, "CREATE");
}

/// Test: Sensitive<T> (ranvier-compliance) preserves data through serialization
/// while redacting in Debug/Display, combined with AuditEvent metadata.
///
/// Crosses: compliance × audit
#[tokio::test]
async fn test_compliance_audit_redaction() {
    let email = Sensitive::new("jane@example.com".to_string());

    // Display/Debug should show [REDACTED]
    let display_output = format!("{email}");
    assert!(display_output.contains("[REDACTED]") || display_output.contains("REDACTED"));

    // Serialize should preserve actual value
    let json = serde_json::to_string(&email).unwrap();
    assert!(json.contains("jane@example.com"));

    // Create audit event with Sensitive data — must use expose() for the actor field
    let event = AuditEvent::new(
        "evt-compliance".into(),
        email.expose().clone(), // Explicit unwrap for authorized access
        "READ".into(),
        "user-profile".into(),
    )
    .with_metadata("email_redacted", format!("{email}"));

    // Verify: event actor has real value (authorized), metadata has redacted form
    assert_eq!(event.actor, "jane@example.com");
    let meta_val = event.metadata.get("email_redacted").unwrap();
    assert!(meta_val.as_str().unwrap().contains("REDACTED"));
}

/// Test: AuditChain hash integrity (ranvier-audit) persists across multiple events
/// with hash chain verification.
///
/// Crosses: audit (chain) × core (types)
#[tokio::test]
async fn test_audit_chain_integrity() {
    let chain = AuditChain::new();

    // Append 5 events
    for i in 0..5 {
        let event = AuditEvent::new(
            format!("chain-evt-{i}"),
            "system".into(),
            "TRANSITION".into(),
            format!("node-{i}"),
        );
        chain.append(event).await;
    }

    assert_eq!(chain.len().await, 5);

    // Chain should verify successfully
    chain
        .verify()
        .await
        .expect("Chain integrity should be valid");

    // Verify events are linked
    let events = chain.events().await;
    assert!(events[0].prev_hash.is_none()); // First event has no predecessor
    for i in 1..5 {
        assert!(
            events[i].prev_hash.is_some(),
            "Event {i} should have prev_hash"
        );
    }
}

/// Test: AccessLogGuard (ranvier-std) writes AccessLogEntry to Bus, which is then
/// readable by downstream transitions in the same Axon pipeline.
///
/// Crosses: std (AccessLogGuard) × runtime (Axon) × core (Bus)
#[tokio::test]
async fn test_access_log_guard_pipeline() {
    let axon = Axon::<UserRequest, UserRequest, String>::new("AccessLogPipeline")
        .then(AccessLogGuard::new().redact_paths(vec!["/auth/login".into()]))
        .then(AccessLogVerifier);

    let mut bus = Bus::new();
    bus.insert(AccessLogRequest {
        method: "GET".into(),
        path: "/api/users".into(),
    });

    let input = UserRequest {
        user_id: "user-2".into(),
        email: "test@example.com".into(),
        action: "LIST".into(),
    };

    let result = axon.execute(input, &(), &mut bus).await;
    assert!(matches!(result, Outcome::Next(_)));

    // Verify AccessLogEntry was written and downstream transition saw it
    assert!(bus.read::<AccessLogEntry>().is_some());
    let verified = bus.read::<String>().cloned();
    assert_eq!(verified.as_deref(), Some("access_log_verified"));
}

/// Test: AccessLogGuard path redaction works in pipeline context.
///
/// Crosses: std × runtime × core (Bus)
#[tokio::test]
async fn test_access_log_redaction_in_pipeline() {
    let axon = Axon::<UserRequest, UserRequest, String>::new("RedactPipeline")
        .then(AccessLogGuard::new().redact_paths(vec!["/auth/login".into()]));

    let mut bus = Bus::new();
    bus.insert(AccessLogRequest {
        method: "POST".into(),
        path: "/auth/login".into(), // This path should be redacted
    });

    let input = UserRequest {
        user_id: "user-3".into(),
        email: "admin@example.com".into(),
        action: "LOGIN".into(),
    };

    let result = axon.execute(input, &(), &mut bus).await;
    assert!(matches!(result, Outcome::Next(_)));

    let entry = bus.read::<AccessLogEntry>().unwrap();
    assert_eq!(entry.path, "[redacted]");
    assert_eq!(entry.method, "POST");
}

/// Test: OpenApiGenerator (ranvier-openapi) with SecurityScheme and ProblemDetail
/// produces valid JSON document with cross-referencing components.
///
/// Crosses: openapi × http (HttpRouteDescriptor) × core (types)
#[test]
fn test_openapi_security_and_problem_detail_combined() {
    use ranvier_http::HttpRouteDescriptor;
    use http::Method;

    let doc = ranvier_openapi::OpenApiGenerator::from_descriptors(vec![
        HttpRouteDescriptor::new(Method::GET, "/api/users"),
        HttpRouteDescriptor::new(Method::POST, "/api/users"),
    ])
    .with_bearer_auth()
    .with_problem_detail_errors()
    .build();

    // Serialize to JSON and parse back
    let json = serde_json::to_string_pretty(&doc).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

    // SecurityScheme should exist
    let schemes = &parsed["components"]["securitySchemes"];
    assert!(schemes["bearerAuth"].is_object());
    assert_eq!(schemes["bearerAuth"]["scheme"], "bearer");

    // ProblemDetail schema should exist
    let schemas = &parsed["components"]["schemas"];
    assert!(schemas["ProblemDetail"].is_object());
    assert!(schemas["ProblemDetail"]["properties"]["status"].is_object());

    // Both routes should have error responses
    let users_get = &parsed["paths"]["/api/users"]["get"];
    assert!(users_get["responses"]["400"].is_object());
    assert!(users_get["responses"]["500"].is_object());

    let users_post = &parsed["paths"]["/api/users"]["post"];
    assert!(users_post["responses"]["400"].is_object());
    assert!(users_post["responses"]["500"].is_object());
}

/// Test: TelemetryConfig (ranvier-core) with init_telemetry() is a no-op when
/// no endpoint is set, and RanvierConfig defaults are sane.
///
/// Crosses: core (config) — validates config system coherence
#[test]
fn test_config_telemetry_default_no_op() {
    let config = RanvierConfig::default();

    // Telemetry should have sane defaults
    assert!(config.telemetry.otlp_endpoint.is_none());
    assert_eq!(config.telemetry.service_name, "ranvier");
    assert!((config.telemetry.sample_ratio - 1.0).abs() < f64::EPSILON);

    // init_telemetry should not panic when no endpoint is set
    config.init_telemetry();
}

/// Test: Full pipeline combining AccessLogGuard + AuditLogger + Compliance types
/// in a single Axon execution.
///
/// Crosses: std × audit × compliance × runtime × core
#[tokio::test]
async fn test_full_operations_pipeline() {
    // Setup audit logger
    let sink = InMemoryAuditSink::new();
    let logger = Arc::new(AuditLogger::new(sink.clone()));

    // Build pipeline: AccessLogGuard → AuditingTransition
    let axon = Axon::<UserRequest, UserRequest, String>::new("FullOpsPipeline")
        .then(AccessLogGuard::new())
        .then(AuditingTransition);

    // Prepare Bus with access log request and audit logger
    let mut bus = Bus::new();
    bus.insert(AccessLogRequest {
        method: "PUT".into(),
        path: "/api/orders/123".into(),
    });
    bus.insert(logger.clone());

    let input = UserRequest {
        user_id: "operator-1".into(),
        email: "ops@example.com".into(),
        action: "UPDATE".into(),
    };

    let result = axon.execute(input, &(), &mut bus).await;
    assert!(matches!(result, Outcome::Next(_)));

    // Verify access log was written
    let entry = bus.read::<AccessLogEntry>().unwrap();
    assert_eq!(entry.method, "PUT");
    assert_eq!(entry.path, "/api/orders/123");

    // Verify audit event was recorded
    let events = sink.get_events().await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].actor, "operator-1");
    assert_eq!(events[0].action, "UPDATE");
}

/// Test: InMemoryAuditSink query with AuditQuery filters works across multiple
/// events from different Axon executions.
///
/// Crosses: audit (query) × runtime × core
#[tokio::test]
async fn test_audit_query_across_executions() {
    let sink = InMemoryAuditSink::new();
    let logger = Arc::new(AuditLogger::new(sink.clone()));

    let axon = Axon::<UserRequest, UserRequest, String>::new("QueryPipeline")
        .then(AuditingTransition);

    // Execute multiple actions
    let actions = vec![
        ("admin", "CREATE"),
        ("admin", "UPDATE"),
        ("viewer", "READ"),
        ("admin", "DELETE"),
    ];

    for (user, action) in &actions {
        let mut bus = Bus::new();
        bus.insert(logger.clone());
        let input = UserRequest {
            user_id: user.to_string(),
            email: format!("{user}@example.com"),
            action: action.to_string(),
        };
        axon.execute(input, &(), &mut bus).await;
    }

    // Verify all events recorded
    assert_eq!(sink.get_events().await.len(), 4);

    // Query by actor
    use ranvier_audit::AuditQuery;
    let query = AuditQuery::new().actor("admin");
    let admin_events = logger.query(&query).await.unwrap();
    assert_eq!(admin_events.len(), 3); // CREATE, UPDATE, DELETE

    // Query by action
    let query = AuditQuery::new().action("READ");
    let read_events = logger.query(&query).await.unwrap();
    assert_eq!(read_events.len(), 1);
    assert_eq!(read_events[0].actor, "viewer");
}
