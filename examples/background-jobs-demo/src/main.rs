//! # Background Jobs Demo
//!
//! Demonstrates periodic Axon execution using `tokio-cron-scheduler`.
//! Replaces the removed `ranvier-job` crate.
//!
//! ## Run
//! ```bash
//! cargo run -p background-jobs-demo
//! ```
//!
//! ## Key Concepts
//! - Use `tokio-cron-scheduler` for cron-based job scheduling
//! - Execute Axon pipelines inside scheduled jobs
//! - No wrapper crate needed — tokio + Axon is sufficient

use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_runtime::Axon;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio_cron_scheduler::{Job, JobScheduler};

// ============================================================================
// Domain
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
struct JobContext {
    job_name: String,
    run_number: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct JobResult {
    job_name: String,
    run_number: u64,
    status: String,
}

// ============================================================================
// Transitions
// ============================================================================

#[derive(Clone)]
struct ProcessJob;

#[async_trait]
impl Transition<JobContext, JobResult> for ProcessJob {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: JobContext,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<JobResult, Self::Error> {
        println!(
            "  [Job] Processing '{}' run #{}",
            input.job_name, input.run_number
        );
        Outcome::next(JobResult {
            job_name: input.job_name,
            run_number: input.run_number,
            status: "completed".into(),
        })
    }
}

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Background Jobs Demo ===\n");

    let axon = Axon::<JobContext, JobContext, String>::new("background-job").then(ProcessJob);

    let counter = Arc::new(AtomicU64::new(0));

    // Create the scheduler
    let mut scheduler = JobScheduler::new()
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    // Schedule a job that runs every 2 seconds
    let axon_clone = axon.clone();
    let counter_clone = counter.clone();

    let job = Job::new_repeated_async(std::time::Duration::from_secs(2), move |_uuid, _lock| {
        let axon = axon_clone.clone();
        let counter = counter_clone.clone();
        Box::pin(async move {
            let run = counter.fetch_add(1, Ordering::SeqCst) + 1;
            let mut bus = Bus::new();
            let ctx = JobContext {
                job_name: "cleanup-task".into(),
                run_number: run,
            };
            let result = axon.execute(ctx, &(), &mut bus).await;
            match &result {
                Outcome::Next(r) => println!(
                    "  [Scheduler] {} #{}: {}",
                    r.job_name, r.run_number, r.status
                ),
                other => println!("  [Scheduler] Unexpected: {:?}", other),
            }
        })
    })
    .map_err(|e| anyhow::anyhow!("{}", e))?;

    scheduler
        .add(job)
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    // Start scheduler
    scheduler
        .start()
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    println!("Scheduler started. Running for 7 seconds...\n");
    tokio::time::sleep(std::time::Duration::from_secs(7)).await;

    // Shutdown
    scheduler
        .shutdown()
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    let total = counter.load(Ordering::SeqCst);
    println!("\nScheduler stopped after {} runs.", total);
    println!("done");
    Ok(())
}
