//! # Distributed Lock Demo
//!
//! Demonstrates Redis leases injected via Bus; effects still require fencing or idempotency.
//! Replaces the removed `ranvier-cluster` crate.
//!
//! ## Run
//! ```bash
//! # Requires Redis: docker run -d -p 6379:6379 redis
//! cargo run -p distributed-lock-demo
//! ```
//!
//! ## Key Concepts
//! - Redis SETNX-based distributed lock implementation
//! - Lock handle injected via Bus for Transition access
//! - TTL expiry and owner-checked release; expiry alone is not safe effect recovery
//! - No wrapper crate needed — `redis` crate + Bus is sufficient

use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_runtime::{Axon, ExecutionReport};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

// ============================================================================
// Distributed Lock
// ============================================================================

/// Local demonstration of a Redis lease, not a fenced singleton guarantee.
#[derive(Clone)]
struct DistributedLockHandle {
    client: redis::Client,
    ttl_ms: u64,
}

struct LockLease {
    key: String,
    token: String,
    deadline: tokio::time::Instant,
}

impl DistributedLockHandle {
    fn new(redis_url: &str, ttl_ms: u64) -> Result<Self, String> {
        // Bound this demo's lease and monotonic deadline arithmetic.
        if ttl_ms == 0 || ttl_ms > 60_000 {
            return Err("Invalid lease duration".into());
        }
        let client = redis::Client::open(redis_url)
            .map_err(|_| "Invalid Redis configuration".to_string())?;
        Ok(Self { client, ttl_ms })
    }

    async fn try_acquire(&self, key: &str) -> Result<Option<LockLease>, String> {
        let token = uuid::Uuid::new_v4().to_string();
        let started = tokio::time::Instant::now();
        let operation = async {
            let mut conn = self
                .client
                .get_multiplexed_async_connection()
                .await
                .map_err(|_| "Redis connection unavailable".to_string())?;
            let reply: Option<String> = redis::cmd("SET")
                .arg(key)
                .arg(&token)
                .arg("NX")
                .arg("PX")
                .arg(self.ttl_ms)
                .query_async(&mut conn)
                .await
                .map_err(|_| "Lock acquisition unconfirmed; reconcile before retry".to_string())?;
            match reply.as_deref() {
                None => Ok(None),
                Some("OK") => Ok(Some(LockLease {
                    key: key.into(),
                    token,
                    deadline: started + std::time::Duration::from_millis(self.ttl_ms),
                })),
                _ => Err("Unexpected lock acknowledgement".into()),
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(2), operation)
            .await
            .map_err(|_| "Lock acquisition timed out; reconcile before retry".to_string())?
    }

    /// Atomic compare-and-delete: a stale owner cannot delete a successor's lease.
    async fn release(&self, lease: &LockLease) -> Result<bool, String> {
        let operation = async {
            let mut conn = self
                .client
                .get_multiplexed_async_connection()
                .await
                .map_err(|_| "Redis connection unavailable".to_string())?;
            let removed: i64 = redis::Script::new(
                "if redis.call('GET', KEYS[1]) == ARGV[1] then return redis.call('DEL', KEYS[1]) else return 0 end"
            ).key(&lease.key).arg(&lease.token).invoke_async(&mut conn).await
                .map_err(|_| "Lock release unconfirmed; reconcile before retry".to_string())?;
            Ok(removed == 1)
        };
        tokio::time::timeout(std::time::Duration::from_secs(2), operation)
            .await
            .map_err(|_| "Lock release timed out; reconcile before retry".to_string())?
    }
}

#[cfg(test)]
include!("web_catalog_fixture.rs");
// ============================================================================
// Transitions
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CriticalTask {
    task_id: String,
    payload: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TaskResult {
    task_id: String,
    status: String,
    lock_acquired: bool,
}

/// Transition that acquires a distributed lock before processing.
#[derive(Clone)]
struct LockedProcessor {
    lock_key: String,
}

#[async_trait]
impl Transition<CriticalTask, TaskResult> for LockedProcessor {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: CriticalTask,
        _resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<TaskResult, Self::Error> {
        let Some(lock) = bus.read::<DistributedLockHandle>() else {
            return Outcome::fault("Lock capability unavailable".into());
        };

        // Try to acquire the distributed lock
        match lock.try_acquire(&self.lock_key).await {
            Ok(Some(lease)) => {
                if tokio::time::Instant::now() >= lease.deadline {
                    return Outcome::fault("Lease expired; reconcile before retry".into());
                }
                println!(
                    "  [Lock] Acquired lock '{}' for task {}",
                    self.lock_key, input.task_id
                );

                // Simulate critical section work
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;

                // Release lock
                let expired = tokio::time::Instant::now() >= lease.deadline;
                match lock.release(&lease).await {
                    Ok(true) if !expired => {}
                    _ => {
                        return Outcome::fault(
                            "Lease completion unconfirmed; reconcile before retry".into(),
                        );
                    }
                }

                Outcome::next(TaskResult {
                    task_id: input.task_id,
                    status: "processed".into(),
                    lock_acquired: true,
                })
            }
            Ok(None) => {
                println!(
                    "  [Lock] Could not acquire lock '{}' — another instance holds it",
                    self.lock_key
                );
                Outcome::next(TaskResult {
                    task_id: input.task_id,
                    status: "skipped_lock_held".into(),
                    lock_acquired: false,
                })
            }
            Err(_) => Outcome::fault("Lock acquisition unconfirmed; reconcile before retry".into()),
        }
    }
}

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Distributed Lock Demo ===\n");
    println!("Note: Requires Redis at redis://127.0.0.1:6379\n");

    let redis_url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".into());

    let lock_handle = match DistributedLockHandle::new(&redis_url, 5000) {
        Ok(h) => h,
        Err(_) => {
            anyhow::bail!("Invalid Redis configuration");
        }
    };

    let axon =
        Axon::<CriticalTask, CriticalTask, String>::new("locked-processor").then(LockedProcessor {
            lock_key: "demo:critical-section".into(),
        });

    // Simulate two concurrent workers trying the same lock
    let axon_a = axon.clone();
    let lock_a = lock_handle.clone();
    let axon_b = axon.clone();
    let lock_b = lock_handle.clone();

    let worker_a = tokio::spawn(async move {
        let mut bus = Bus::new();
        bus.insert(lock_a);
        axon_a
            .execute(
                CriticalTask {
                    task_id: "task-A".into(),
                    payload: "critical data A".into(),
                },
                &(),
                &mut bus,
            )
            .await
    });

    let worker_b = tokio::spawn(async move {
        let mut bus = Bus::new();
        bus.insert(lock_b);
        axon_b
            .execute(
                CriticalTask {
                    task_id: "task-B".into(),
                    payload: "critical data B".into(),
                },
                &(),
                &mut bus,
            )
            .await
    });

    let (result_a, result_b) = tokio::join!(worker_a, worker_b);
    for result in [result_a, result_b] {
        let report =
            result.map_err(|_| anyhow::anyhow!("Worker failed; reconcile before retry"))?;
        let result = local_demo_result(report)?;
        println!("  Worker status: {}", result.status);
    }

    println!("\ndone");
    Ok(())
}

fn local_demo_result(
    report: ExecutionReport<Outcome<TaskResult, String>>,
) -> anyhow::Result<TaskResult> {
    if report
        .cleanup()
        .is_some_and(|cleanup| !cleanup.is_complete())
    {
        anyhow::bail!("Execution requires reconciliation before retry");
    }
    match report {
        ExecutionReport::Unpersisted {
            result: Outcome::Next(result),
            ..
        } => Ok(result),
        _ => anyhow::bail!("Execution requires reconciliation before retry"),
    }
}

#[cfg(test)]
mod report_tests {
    use super::*;
    use ranvier_runtime::{CompletionConfirmation as C, EffectBoundary, PersistenceOperation, SagaRollbackReport};
    #[test]
    fn local_policy_rejects_unconfirmed_and_incomplete_results() {
        let next = || Outcome::Next(TaskResult { task_id: "synthetic".into(), status: "processed".into(), lock_acquired: true });
        assert!(local_demo_result(ExecutionReport::Unpersisted { result: next(), cleanup: None }).is_ok());
        for report in [
            ExecutionReport::Unpersisted { result: Outcome::Fault("private".into()), cleanup: None },
            ExecutionReport::Unpersisted { result: next(), cleanup: Some(SagaRollbackReport { trace_id: "private".into(), finished: false, steps: vec![] }) },
            ExecutionReport::NotRequested { result: next(), cleanup: None },
            ExecutionReport::Recorded { result: next(), completion: C::Confirmed, cleanup: None },
            ExecutionReport::Recorded { result: next(), completion: C::NotRequested, cleanup: None },
            ExecutionReport::Unconfirmed { result: next(), operation: PersistenceOperation::Result, effect: EffectBoundary::MayHaveRun, cleanup: None },
            ExecutionReport::BlockedBeforeEffect { operation: PersistenceOperation::Intent, effect: EffectBoundary::NotInvoked, cleanup: None },
        ] { assert_eq!(local_demo_result(report).unwrap_err().to_string(), "Execution requires reconciliation before retry"); }
    }
}
