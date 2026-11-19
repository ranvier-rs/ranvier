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
use ranvier_runtime::{Axon, ExecutionReport};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio_cron_scheduler::{Job, JobScheduler};

fn local_job_succeeded<T, E>(report: ExecutionReport<Outcome<T, E>>) -> bool {
    !report
        .cleanup()
        .is_some_and(|cleanup| !cleanup.is_complete())
        && matches!(
            report,
            ExecutionReport::Unpersisted {
                result: Outcome::Next(_),
                ..
            }
        )
}

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
    // Local overlap guard and fail-closed latch; this is not a distributed lock.
    let halted = Arc::new(tokio::sync::Mutex::new(false));

    // Create the scheduler
    let mut scheduler = JobScheduler::new()
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    // Schedule a job that runs every 2 seconds
    let axon_clone = axon.clone();
    let counter_clone = counter.clone();
    let halted_clone = halted.clone();

    let job = Job::new_repeated_async(std::time::Duration::from_secs(2), move |_uuid, _lock| {
        let axon = axon_clone.clone();
        let counter = counter_clone.clone();
        let halted = halted_clone.clone();
        Box::pin(async move {
            let Ok(mut stopped) = halted.try_lock() else {
                return;
            };
            if *stopped {
                return;
            }
            // A panic or cancelled callback leaves future ticks stopped.
            *stopped = true;
            // Checked increment without `fetch_update` (deprecated on newer stable)
            // or `try_update` (unstable on the 1.93 MSRV).
            let mut current = counter.load(Ordering::SeqCst);
            let previous = loop {
                let Some(next) = current.checked_add(1) else {
                    return;
                };
                match counter.compare_exchange_weak(
                    current,
                    next,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    Ok(previous) => break previous,
                    Err(actual) => current = actual,
                }
            };
            let run = previous + 1;
            let mut bus = Bus::new();
            let ctx = JobContext {
                job_name: "cleanup-task".into(),
                run_number: run,
            };
            let report = axon.execute(ctx, &(), &mut bus).await;
            if local_job_succeeded(report) {
                *stopped = false;
                println!("  [Scheduler] Completed local run #{}", run);
            } else {
                eprintln!("  [Scheduler] Stopped; reconcile before restarting");
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

#[cfg(test)]
include!("web_catalog_fixture.rs");

#[cfg(test)]
mod policy_tests {
    use super::*;
    use ranvier_runtime::{
        CompletionConfirmation, EffectBoundary, PersistenceOperation, SagaRollbackOutcome as O,
        SagaRollbackReport, SagaRollbackStep,
    };
    #[test]
    fn periodic_policy_rejects_uncertain_and_non_next_reports() {
        let next = || Outcome::<u32, String>::Next(7);
        assert!(local_job_succeeded(ExecutionReport::Unpersisted {
            result: next(),
            cleanup: None
        }));
        for report in [
            ExecutionReport::Unpersisted {
                result: Outcome::Fault("private".into()),
                cleanup: None,
            },
            ExecutionReport::Unpersisted {
                result: Outcome::Emit("private".into(), None),
                cleanup: None,
            },
            ExecutionReport::NotRequested {
                result: next(),
                cleanup: None,
            },
            ExecutionReport::Recorded {
                result: next(),
                completion: CompletionConfirmation::Confirmed,
                cleanup: None,
            },
            ExecutionReport::Recorded {
                result: next(),
                completion: CompletionConfirmation::NotRequested,
                cleanup: None,
            },
            ExecutionReport::Unconfirmed {
                result: next(),
                operation: PersistenceOperation::Result,
                effect: EffectBoundary::MayHaveRun,
                cleanup: None,
            },
            ExecutionReport::BlockedBeforeEffect {
                operation: PersistenceOperation::Intent,
                effect: EffectBoundary::NotInvoked,
                cleanup: None,
            },
        ] {
            assert!(!local_job_succeeded(report));
        }
    }
    #[test]
    fn periodic_policy_requires_complete_cleanup() {
        for finished in [false, true] {
            for outcome in [
                O::InProgress,
                O::Panicked,
                O::Completed,
                O::Fault,
                O::Emitted,
                O::Branched,
                O::Jumped,
                O::MissingHandler,
            ] {
                assert_eq!(
                    local_job_succeeded(ExecutionReport::Unpersisted {
                        result: Outcome::<u32, String>::Next(7),
                        cleanup: Some(SagaRollbackReport {
                            trace_id: "private".into(),
                            finished,
                            steps: vec![SagaRollbackStep {
                                node_id: "private".into(),
                                outcome
                            }]
                        }),
                    }),
                    finished && outcome == O::Completed
                );
            }
        }
    }
}
