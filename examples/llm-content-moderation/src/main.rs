//! # LLM Content Moderation
//!
//! Demonstrates the LLM Integration Pattern using Ranvier transitions.
//!
//! A 3-stage pipeline classifies user content with a mock LLM and applies
//! business-rule policy to approve, flag, or reject submissions.
//!
//! ## Pipeline
//! ```text
//! ExtractContent → ModerateContent → ApplyPolicy
//! ```
//!
//! ## Run
//! ```bash
//! cargo run -p llm-content-moderation
//! ```
//!
//! ## Endpoints
//! - POST /moderate — submit content for AI moderation
//! - GET  /health   — health check

mod models;
mod transitions;

use anyhow::Result;
use ranvier_http::Ranvier;
use ranvier_runtime::Axon;

use transitions::{
    extract_content::extract_content,
    moderate::moderate_content,
    apply_policy::apply_policy,
};

/// Build the 3-stage content-moderation pipeline.
///
/// Flow: ExtractContent → ModerateContent → ApplyPolicy
fn moderation_circuit() -> Axon<(), serde_json::Value, String> {
    Axon::simple::<String>("content-moderation")
        .then(extract_content)
        .then(moderate_content)
        .then(apply_policy)
}

/// Simple health-check circuit.
fn health_circuit() -> Axon<(), serde_json::Value, String> {
    use ranvier_core::prelude::*;
    use ranvier_macros::transition;

    #[transition]
    async fn health_check(
        _input: (),
        _res: &(),
        _bus: &mut Bus,
    ) -> Outcome<serde_json::Value, String> {
        Outcome::Next(serde_json::json!({
            "status": "ok",
            "service": "llm-content-moderation",
        }))
    }

    Axon::simple::<String>("health").then(health_check)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".to_string());

    println!("LLM Content Moderation API starting on {addr}");
    println!("  POST /moderate  — submit content for AI moderation");
    println!("  GET  /health    — health check");
    println!();
    println!("Pipeline: ExtractContent -> ModerateContent (mock LLM) -> ApplyPolicy");

    Ranvier::http()
        .bind(&addr)
        .post("/moderate", moderation_circuit())
        .get("/health", health_circuit())
        .run(())
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    Ok(())
}
