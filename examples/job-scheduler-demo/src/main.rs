use ranvier_core::prelude::*;
use ranvier_macros::transition;
use ranvier_runtime::Axon;
use ranvier_job::prelude::*;
use ranvier_job::job::AxonJob;
use std::time::Duration;
use tokio::signal;
use tracing::{info, Level};

/// A dummy transition that simulates a periodic health check.
#[transition]
async fn health_check(_state: (), _resources: &(), _bus: &mut Bus) -> Outcome<String, String> {
    info!("Running routine health check...");
    tokio::time::sleep(Duration::from_millis(100)).await;
    Outcome::Next("Health Check OK".to_string())
}

/// A dummy transition that simulates a background database cleanup job.
#[transition]
async fn cleanup_job(_state: (), _resources: &(), _bus: &mut Bus) -> Outcome<String, String> {
    info!("Starting database cleanup...");
    tokio::time::sleep(Duration::from_millis(500)).await;
    Outcome::Next("Cleanup complete".to_string())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing subscriber
    tracing_subscriber::fmt()
        .with_max_level(Level::DEBUG)
        .init();

    info!("Starting Ranvier Background Scheduler Demo");

    // Create the scheduler
    let scheduler = Scheduler::new();
    let shutdown_token = scheduler.shutdown_token();

    // 1. Register an interval-based health check job (every 2 seconds)
    let health_axon = Axon::<(), (), String>::new("HealthCheck").then(health_check);
    let health_trigger = Trigger::interval(Duration::from_secs(2));
    let health_job = AxonJob::new("health-check-1", health_trigger, health_axon, (), ());
    
    scheduler.add_job(health_job).await;

    // 2. Register a cron-based cleanup job (every 5 seconds)
    let cleanup_axon = Axon::<(), (), String>::new("CleanupTask").then(cleanup_job);
    // Standard 6-part cron: sec min hour dom month dow
    let cleanup_trigger = Trigger::cron("*/5 * * * * *")?;
    let cleanup_job_task = AxonJob::new("cleanup-db", cleanup_trigger, cleanup_axon, (), ());

    scheduler.add_job(cleanup_job_task).await;

    // Start the scheduler loop in the background
    let scheduler_task = tokio::spawn(async move {
        scheduler.start().await;
    });

    // Wait for Ctrl+C
    match signal::ctrl_c().await {
        Ok(()) => {
            info!("Ctrl+C received, initiating graceful shutdown...");
            shutdown_token.cancel();
        },
        Err(err) => {
            tracing::error!("Unable to listen for shutdown signal: {}", err);
        },
    }

    // Wait for the scheduler to finish awaiting all running jobs
    let _ = scheduler_task.await;

    info!("Demo application exited gracefully.");
    Ok(())
}
