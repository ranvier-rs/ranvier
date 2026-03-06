//! # Cluster Demo
//!
//! Demonstrates distributed primitives using the Ranvier Cluster traits:
//! `DistributedLock`, `DistributedStore`, and `ClusterBus`.
//!
//! This demo uses **in-memory mock implementations** so it runs without
//! any external infrastructure (Redis, etc.). In production, swap the
//! mock types for `RedisDistributedLock` / `RedisClusterBus` from the
//! `ranvier-cluster` crate with the `redis` feature enabled.
//!
//! ## Run
//! ```bash
//! cargo run -p cluster-demo
//! ```
//!
//! ## Key Concepts
//! - `DistributedLock` — acquire / release / extend distributed locks
//! - `DistributedStore` — key-value get / put / delete with optional TTL
//! - `ClusterBus` — publish / subscribe for inter-node coordination
//! - Trait-based design allows swapping backends without changing app code
//!
//! ## Production Usage (Redis)
//! ```rust,ignore
//! use ranvier_cluster::prelude::*;
//! // Enable the `redis` feature in Cargo.toml:
//! //   ranvier-cluster = { workspace = true, features = ["redis"] }
//! let pool = bb8::Pool::builder()
//!     .build(bb8_redis::RedisConnectionManager::new("redis://127.0.0.1/")?)
//!     .await?;
//! let lock = RedisDistributedLock::new(pool.clone(), "node-1");
//! let bus  = RedisClusterBus::new(pool);
//! ```

use async_trait::async_trait;
use ranvier_core::cluster::{ClusterBus, ClusterError, DistributedLock, DistributedStore};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// ── In-Memory Mock Implementations ──────────────────────────────────────

/// In-memory distributed lock for demo/testing purposes.
#[derive(Clone, Default)]
struct InMemoryLock {
    locks: Arc<Mutex<HashMap<String, String>>>,
    node_id: String,
}

impl InMemoryLock {
    fn new(node_id: impl Into<String>) -> Self {
        Self {
            locks: Arc::new(Mutex::new(HashMap::new())),
            node_id: node_id.into(),
        }
    }
}

#[async_trait]
impl DistributedLock for InMemoryLock {
    async fn try_acquire(&self, key: &str, _ttl_ms: u64) -> Result<bool, ClusterError> {
        let mut map = self.locks.lock().unwrap();
        if map.contains_key(key) {
            Ok(false) // Already held
        } else {
            map.insert(key.to_string(), self.node_id.clone());
            Ok(true)
        }
    }

    async fn release(&self, key: &str) -> Result<(), ClusterError> {
        let mut map = self.locks.lock().unwrap();
        if map.get(key).is_some_and(|owner| owner == &self.node_id) {
            map.remove(key);
            Ok(())
        } else {
            Err(ClusterError::LockReleaseFailed(format!(
                "Lock '{key}' not held by node '{}'",
                self.node_id
            )))
        }
    }

    async fn extend(&self, key: &str, _extra_ttl_ms: u64) -> Result<(), ClusterError> {
        let map = self.locks.lock().unwrap();
        if map.get(key).is_some_and(|owner| owner == &self.node_id) {
            Ok(()) // TTL extension is a no-op in memory
        } else {
            Err(ClusterError::LockHeld(format!(
                "Lock '{key}' not held by node '{}'",
                self.node_id
            )))
        }
    }
}

/// In-memory distributed store for demo/testing purposes.
#[derive(Clone, Default)]
struct InMemoryStore {
    data: Arc<Mutex<HashMap<String, Vec<u8>>>>,
}

#[async_trait]
impl DistributedStore for InMemoryStore {
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, ClusterError> {
        let map = self.data.lock().unwrap();
        Ok(map.get(key).cloned())
    }

    async fn put(&self, key: &str, value: &[u8], _ttl_ms: Option<u64>) -> Result<(), ClusterError> {
        let mut map = self.data.lock().unwrap();
        map.insert(key.to_string(), value.to_vec());
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<(), ClusterError> {
        let mut map = self.data.lock().unwrap();
        map.remove(key);
        Ok(())
    }
}

/// In-memory cluster bus for demo/testing purposes.
#[derive(Clone, Default)]
struct InMemoryBus {
    subscriptions: Arc<Mutex<Vec<String>>>,
    messages: Arc<Mutex<Vec<(String, Vec<u8>)>>>,
}

#[async_trait]
impl ClusterBus for InMemoryBus {
    async fn publish(&self, topic: &str, payload: &[u8]) -> Result<(), ClusterError> {
        let mut msgs = self.messages.lock().unwrap();
        msgs.push((topic.to_string(), payload.to_vec()));
        Ok(())
    }

    async fn subscribe(&self, topic: &str) -> Result<(), ClusterError> {
        let mut subs = self.subscriptions.lock().unwrap();
        subs.push(topic.to_string());
        Ok(())
    }
}

// ── Main ──────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    println!("=== Ranvier Cluster Demo (in-memory mock) ===");
    println!();

    // ── 1. Distributed Lock ────────────────────────────────────────
    println!("--- Distributed Lock ---");

    let lock_a = InMemoryLock::new("node-alpha");
    let lock_b = InMemoryLock {
        locks: lock_a.locks.clone(), // Share the same lock state
        node_id: "node-beta".to_string(),
    };

    // Node A acquires the lock
    let acquired = lock_a.try_acquire("job:daily-report", 5000).await?;
    println!("  node-alpha acquire 'job:daily-report': {acquired}");

    // Node B tries to acquire the same lock — should fail
    let acquired = lock_b.try_acquire("job:daily-report", 5000).await?;
    println!("  node-beta  acquire 'job:daily-report': {acquired}");

    // Node A extends the lock TTL
    lock_a.extend("job:daily-report", 5000).await?;
    println!("  node-alpha extend  'job:daily-report': ok");

    // Node A releases the lock
    lock_a.release("job:daily-report").await?;
    println!("  node-alpha release 'job:daily-report': ok");

    // Now Node B can acquire it
    let acquired = lock_b.try_acquire("job:daily-report", 5000).await?;
    println!("  node-beta  acquire 'job:daily-report': {acquired}");
    lock_b.release("job:daily-report").await?;

    println!();

    // ── 2. Distributed Store ───────────────────────────────────────
    println!("--- Distributed Store ---");

    let store = InMemoryStore::default();

    // Put and get a value
    let config = serde_json::json!({"max_retries": 3, "timeout_ms": 5000});
    store
        .put("config:pipeline-a", config.to_string().as_bytes(), None)
        .await?;
    println!("  put 'config:pipeline-a': {config}");

    let value = store.get("config:pipeline-a").await?;
    if let Some(bytes) = value {
        let retrieved: serde_json::Value = serde_json::from_slice(&bytes)?;
        println!("  get 'config:pipeline-a': {retrieved}");
    }

    // Delete
    store.delete("config:pipeline-a").await?;
    let deleted = store.get("config:pipeline-a").await?;
    println!("  delete + get: {:?}", deleted);

    println!();

    // ── 3. Cluster Bus ─────────────────────────────────────────────
    println!("--- Cluster Bus ---");

    let bus = InMemoryBus::default();

    bus.subscribe("ranvier:schema-updates").await?;
    println!("  subscribed to 'ranvier:schema-updates'");

    bus.publish(
        "ranvier:schema-updates",
        b"{\"event\":\"schema_changed\",\"circuit\":\"OrderPipeline\"}",
    )
    .await?;
    println!("  published schema_changed event");

    bus.publish(
        "ranvier:node-health",
        b"{\"node\":\"node-alpha\",\"status\":\"healthy\"}",
    )
    .await?;
    println!("  published node-health event");

    // Show published messages
    let msgs = bus.messages.lock().unwrap();
    println!("  total published messages: {}", msgs.len());
    for (topic, payload) in msgs.iter() {
        let payload_str = String::from_utf8_lossy(payload);
        println!("    [{topic}] {payload_str}");
    }

    println!();
    println!("=== Demo complete ===");
    println!();
    println!("To use with real Redis, enable the `redis` feature:");
    println!("  ranvier-cluster = {{ workspace = true, features = [\"redis\"] }}");

    Ok(())
}
