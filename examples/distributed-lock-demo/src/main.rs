//! # Distributed Lock Demo
//!
//! Demonstrates Redis-based distributed locking injected via Bus for Axon singleton execution.
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
//! - Automatic TTL-based lock expiry for crash recovery
//! - No wrapper crate needed — `redis` crate + Bus is sufficient

use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_runtime::Axon;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

// ============================================================================
// Distributed Lock
// ============================================================================

/// Simple Redis-based distributed lock.
#[derive(Clone)]
struct DistributedLockHandle {
    client: Arc<Mutex<Option<redis::Client>>>,
    ttl_ms: u64,
}

impl DistributedLockHandle {
    fn new(redis_url: &str, ttl_ms: u64) -> Result<Self, String> {
        let client = redis::Client::open(redis_url).map_err(|e| e.to_string())?;
        Ok(Self {
            client: Arc::new(Mutex::new(Some(client))),
            ttl_ms,
        })
    }

    /// Try to acquire a lock. Returns true if acquired.
    async fn try_acquire(&self, key: &str) -> Result<bool, String> {
        let guard = self.client.lock().await;
        let client = guard.as_ref().ok_or("Redis client not available")?;
        let mut conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| format!("Redis connect: {}", e))?;

        let result: bool = redis::cmd("SET")
            .arg(key)
            .arg("locked")
            .arg("NX")
            .arg("PX")
            .arg(self.ttl_ms)
            .query_async(&mut conn)
            .await
            .unwrap_or(false);

        Ok(result)
    }

    /// Release the lock.
    async fn release(&self, key: &str) -> Result<(), String> {
        let guard = self.client.lock().await;
        let client = guard.as_ref().ok_or("Redis client not available")?;
        let mut conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| format!("Redis connect: {}", e))?;

        let _: () = redis::cmd("DEL")
            .arg(key)
            .query_async(&mut conn)
            .await
            .map_err(|e| format!("Redis DEL: {}", e))?;

        Ok(())
    }
}

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
    lock_held: bool,
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
        let lock = bus
            .read::<DistributedLockHandle>()
            .expect("DistributedLockHandle must be in Bus");

        // Try to acquire the distributed lock
        match lock.try_acquire(&self.lock_key).await {
            Ok(true) => {
                println!(
                    "  [Lock] Acquired lock '{}' for task {}",
                    self.lock_key, input.task_id
                );

                // Simulate critical section work
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;

                // Release lock
                if let Err(e) = lock.release(&self.lock_key).await {
                    println!("  [Lock] Warning: failed to release lock: {}", e);
                }

                Outcome::next(TaskResult {
                    task_id: input.task_id,
                    status: "processed".into(),
                    lock_held: true,
                })
            }
            Ok(false) => {
                println!(
                    "  [Lock] Could not acquire lock '{}' — another instance holds it",
                    self.lock_key
                );
                Outcome::next(TaskResult {
                    task_id: input.task_id,
                    status: "skipped_lock_held".into(),
                    lock_held: false,
                })
            }
            Err(e) => {
                println!("  [Lock] Redis error: {}", e);
                Outcome::fault(format!("Lock acquisition failed: {}", e))
            }
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
        Err(e) => {
            println!("Could not create Redis client: {}", e);
            println!("Skipping demo (Redis not available).");
            return Ok(());
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
    println!("\n  Worker A: {:?}", result_a.unwrap());
    println!("  Worker B: {:?}", result_b.unwrap());

    println!("\ndone");
    Ok(())
}
