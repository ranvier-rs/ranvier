//! otel-ops-demo (M154: OTel Ops Playbook)
//!
//! A standalone demonstration of OTel operational policy patterns:
//!
//! 1. Environment-aware configuration (dev/staging/prod)
//! 2. Redaction mode selection (`RANVIER_REDACT_MODE` env var)
//! 3. Tenant-tagged structured logging / span events
//! 4. Policy gate: reject strict mode in production
//!
//! ## Running
//!
//! ```bash
//! # Dev (console output, no policy enforcement)
//! cargo run -p otel-ops-demo
//!
//! # Staging (policy=ok, internal redaction)
//! RANVIER_ENV=staging RANVIER_REDACT_MODE=internal cargo run -p otel-ops-demo
//!
//! # Prod policy violation demo
//! RANVIER_ENV=prod RANVIER_REDACT_MODE=strict cargo run -p otel-ops-demo
//! ```
//!
//! See `docs/03_guides/otel_ops_playbook.md` for the full operational guide.

use anyhow::{anyhow, Result};

// ── Policy config ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
enum OpsEnv {
    Dev,
    Staging,
    Prod,
}

impl OpsEnv {
    fn from_env() -> Self {
        match std::env::var("RANVIER_ENV")
            .unwrap_or_default()
            .to_lowercase()
            .as_str()
        {
            "staging" => OpsEnv::Staging,
            "prod" | "production" => OpsEnv::Prod,
            _ => OpsEnv::Dev,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RedactMode {
    /// All fields retained — dev only
    Strict,
    /// PII stripped at Collector — internal platforms
    Internal,
    /// All user-identifiable fields stripped — 3rd-party SaaS backends
    Public,
}

impl RedactMode {
    fn from_env() -> Self {
        match std::env::var("RANVIER_REDACT_MODE")
            .unwrap_or_default()
            .to_lowercase()
            .as_str()
        {
            "public" => RedactMode::Public,
            "internal" => RedactMode::Internal,
            _ => RedactMode::Strict,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            RedactMode::Strict => "strict (dev-only — no redaction)",
            RedactMode::Internal => "internal (PII stripped at Collector)",
            RedactMode::Public => "public (all user fields stripped)",
        }
    }
}

// ── Simulated observability pipeline ─────────────────────────────────────────

struct TenantReq {
    tenant_id: String,
    user_id: String,
    trace_id: String,
}

/// Handle a single tenant request: emit trace event, apply policy gate.
///
/// In a real Ranvier service this would be a Transition in an Axon pipeline.
/// Tracing events here map to OTel spans when OTLP exporter is configured.
async fn process_request(req: &TenantReq, env: &OpsEnv, redact: &RedactMode) -> Result<String> {
    // Step 1: emit span event (user_id filtered at Collector in Public mode)
    tracing::info!(
        tenant_id = %req.tenant_id,
        user_id = %req.user_id,   // stripped by Collector redaction processor in public mode
        trace_id = %req.trace_id,
        env = ?env,
        redact_mode = %redact.label(),
        "request handled"
    );

    let response = format!("tenant={} trace={}", req.tenant_id, req.trace_id);

    // Step 2: policy gate — prod must not use strict mode
    if *env == OpsEnv::Prod && *redact == RedactMode::Strict {
        tracing::warn!(
            tenant_id = %req.tenant_id,
            "POLICY VIOLATION: strict redaction not allowed in production"
        );
        return Err(anyhow!(
            "Policy violation: RANVIER_REDACT_MODE=strict is not allowed in prod. \
             Use 'public' or 'internal'."
        ));
    }

    tracing::info!(
        tenant_id = %req.tenant_id,
        redact_mode = %redact.label(),
        "policy check passed"
    );

    Ok(format!("{response} [policy=ok redact={}]", redact.label()))
}

// ── Main ───────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    // In production: replace with OTLP exporter setup
    // export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317
    // export OTEL_SERVICE_NAME=otel-ops-demo
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse()?),
        )
        .init();

    let env = OpsEnv::from_env();
    let redact = RedactMode::from_env();

    println!("=== OTel Ops Demo (M154) ===");
    println!("Environment  : {env:?}");
    println!("Redact Mode  : {}", redact.label());
    println!();

    tracing::info!(env = ?env, redact_mode = %redact.label(), "demo starting");

    // Simulate 3 tenant requests
    let tenants = [
        ("tenant-1", "user-10", "trace-1"),
        ("tenant-2", "user-20", "trace-2"),
        ("tenant-3", "user-30", "trace-3"),
    ];

    let mut had_error = false;
    for (tenant_id, user_id, trace_id) in tenants {
        let req = TenantReq {
            tenant_id: tenant_id.into(),
            user_id: user_id.into(),
            trace_id: trace_id.into(),
        };
        match process_request(&req, &env, &redact).await {
            Ok(result) => println!("[{tenant_id}] OK   : {result}"),
            Err(err) => {
                eprintln!("[{tenant_id}] FAULT: {err}");
                had_error = true;
                if env == OpsEnv::Prod {
                    return Err(err);
                }
            }
        }
    }

    println!();
    if had_error {
        println!("Some requests failed — see FAULT lines above.");
        println!("Fix: set RANVIER_REDACT_MODE=public or internal for prod.");
    } else {
        println!("All requests processed. Check trace output above.");
    }
    println!();
    println!("Sending traces to a real backend:");
    println!("  See docs/03_guides/otel_ops_playbook.md §4 for environment setup.");
    println!("  See docs/03_guides/otel_vendor_configs/ for vendor configs.");

    Ok(())
}
