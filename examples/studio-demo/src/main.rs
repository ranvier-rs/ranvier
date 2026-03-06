//! # Ranvier Studio Inspector Demo
//!
//! Demonstrates real-time tracing, schematic export, and inspector integration for Studio.
//!
//! ## Run
//! ```bash
//! cargo run -p studio-demo
//! ```
//!
//! ## Key Concepts
//! - Inspector layer for distributed tracing
//! - Schematic export mode for circuit visualization
//! - Timeline output for post-hoc analysis

use ranvier_core::prelude::*;
use ranvier_macros::transition;
use ranvier_runtime::Axon;
use std::time::Duration;

#[transition]
async fn step_one(input: i32) -> Outcome<i32, String> {
    Outcome::Next(input + 10)
}

#[transition]
async fn step_two(input: i32) -> Outcome<String, String> {
    Outcome::Next(format!("Result: {}", input))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use tracing_subscriber::prelude::*;

    let fmt_layer = tracing_subscriber::fmt::layer();
    let inspector_layer = ranvier_inspector::layer();

    tracing_subscriber::registry()
        .with(fmt_layer)
        .with(inspector_layer)
        .init();

    tracing::info!("Starting Studio Demo...");

    let info_axon = Axon::<i32, i32, String>::new("Studio Demo Circuit")
        .then(step_one)
        .then(step_two);

    if info_axon.maybe_export_and_exit_with(|request| {
        tracing::info!(
            "Schematic mode requested. Skipping inspector bootstrap and runtime loop. output={:?}",
            request.output
        );
    })? {
        return Ok(());
    }

    let axon = info_axon.serve_inspector(9000);

    tracing::info!("Inspector mode: RANVIER_MODE=dev|prod, enabled by RANVIER_INSPECTOR=1|0");
    tracing::info!("Inspector dev page: http://localhost:9000/quick-view");
    tracing::info!("Raw endpoints: /schematic, /trace/public, /trace/internal (dev only)");

    loop {
        tracing::info!("Executing Axon...");
        let _ = axon.execute(50, &(), &mut Bus::new()).await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}
