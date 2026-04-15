//! Persistent trace storage for Inspector production deployments.
//!
//! The `TraceStore` trait defines the interface for saving and querying traces.
//! `InMemoryTraceStore` provides a bounded in-memory implementation.
//! For production, use `SqliteTraceStore` (requires `trace-sqlite` feature).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A completed trace record suitable for persistent storage.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredTrace {
    pub trace_id: String,
    pub circuit: String,
    pub status: String,
    pub started_at: u64,
    pub finished_at: u64,
    pub duration_ms: u64,
    pub outcome_type: Option<String>,
    pub node_count: usize,
    pub fault_count: usize,
    /// Optional JSON payload of the full trace timeline.
    pub timeline_json: Option<String>,
}

/// Query filter for trace retrieval.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TraceQuery {
    /// Filter by circuit name (exact match).
    pub circuit: Option<String>,
    /// Filter by status (completed, faulted).
    pub status: Option<String>,
    /// Only return traces started after this timestamp (epoch ms).
    pub from: Option<u64>,
    /// Only return traces started before this timestamp (epoch ms).
    pub to: Option<u64>,
    /// Maximum number of results (default: 100).
    pub limit: Option<usize>,
}

/// Retention policy for automatic trace cleanup.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RetentionPolicy {
    /// Maximum number of stored traces.
    pub max_count: usize,
    /// Maximum age in seconds. Traces older than this are pruned.
    pub max_age_secs: u64,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            max_count: 10_000,
            max_age_secs: 7 * 24 * 3600, // 7 days
        }
    }
}

/// Trait for persistent trace storage backends.
#[async_trait]
pub trait TraceStore: Send + Sync {
    /// Save a completed trace.
    async fn save(&self, trace: StoredTrace) -> Result<(), String>;

    /// Query stored traces with optional filters.
    async fn query(&self, filter: TraceQuery) -> Result<Vec<StoredTrace>, String>;

    /// Get a single trace by ID.
    async fn get(&self, trace_id: &str) -> Result<Option<StoredTrace>, String>;

    /// Count total stored traces.
    async fn count(&self) -> Result<usize, String>;

    /// Apply retention policy: prune old or excess traces.
    async fn apply_retention(&self, policy: &RetentionPolicy) -> Result<usize, String>;
}

/// Bounded in-memory trace store for development and testing.
///
/// Uses a `VecDeque` ring buffer for O(1) push/pop and automatic TTL-based pruning.
pub struct InMemoryTraceStore {
    traces: std::sync::Mutex<std::collections::VecDeque<StoredTrace>>,
    max_count: usize,
    /// TTL in milliseconds. Traces older than this are pruned on save.
    trace_ttl_ms: u64,
}

impl InMemoryTraceStore {
    pub fn new(max_count: usize) -> Self {
        Self {
            traces: std::sync::Mutex::new(std::collections::VecDeque::new()),
            max_count,
            trace_ttl_ms: 3_600_000, // 1 hour default
        }
    }

    /// Create with custom TTL (in seconds).
    pub fn with_ttl(max_count: usize, ttl_secs: u64) -> Self {
        Self {
            traces: std::sync::Mutex::new(std::collections::VecDeque::new()),
            max_count,
            trace_ttl_ms: ttl_secs * 1000,
        }
    }

    /// Prune expired traces from the front of the deque.
    fn prune_expired(traces: &mut std::collections::VecDeque<StoredTrace>, ttl_ms: u64) {
        if ttl_ms == 0 {
            return;
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let cutoff = now_ms.saturating_sub(ttl_ms);
        while let Some(front) = traces.front() {
            if front.started_at < cutoff {
                traces.pop_front();
            } else {
                break;
            }
        }
    }
}

impl Default for InMemoryTraceStore {
    fn default() -> Self {
        Self::new(1000)
    }
}

#[async_trait]
impl TraceStore for InMemoryTraceStore {
    async fn save(&self, trace: StoredTrace) -> Result<(), String> {
        let mut traces = self.traces.lock().map_err(|e| e.to_string())?;

        // Prune expired traces before inserting
        Self::prune_expired(&mut traces, self.trace_ttl_ms);

        traces.push_back(trace);
        // Trim from the front if over capacity
        while traces.len() > self.max_count {
            traces.pop_front();
        }
        Ok(())
    }

    async fn query(&self, filter: TraceQuery) -> Result<Vec<StoredTrace>, String> {
        let traces = self.traces.lock().map_err(|e| e.to_string())?;
        let limit = filter.limit.unwrap_or(100);

        let result: Vec<StoredTrace> = traces
            .iter()
            .rev()
            .filter(|t| {
                if let Some(ref circuit) = filter.circuit {
                    if &t.circuit != circuit {
                        return false;
                    }
                }
                if let Some(ref status) = filter.status {
                    if &t.status != status {
                        return false;
                    }
                }
                if let Some(from) = filter.from {
                    if t.started_at < from {
                        return false;
                    }
                }
                if let Some(to) = filter.to {
                    if t.started_at > to {
                        return false;
                    }
                }
                true
            })
            .take(limit)
            .cloned()
            .collect();

        Ok(result)
    }

    async fn get(&self, trace_id: &str) -> Result<Option<StoredTrace>, String> {
        let traces = self.traces.lock().map_err(|e| e.to_string())?;
        Ok(traces.iter().find(|t| t.trace_id == trace_id).cloned())
    }

    async fn count(&self) -> Result<usize, String> {
        let traces = self.traces.lock().map_err(|e| e.to_string())?;
        Ok(traces.len())
    }

    async fn apply_retention(&self, policy: &RetentionPolicy) -> Result<usize, String> {
        let mut traces = self.traces.lock().map_err(|e| e.to_string())?;
        let before = traces.len();

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let cutoff = now_ms.saturating_sub(policy.max_age_secs * 1000);

        // Prune from front (oldest first) by age
        while let Some(front) = traces.front() {
            if front.started_at < cutoff {
                traces.pop_front();
            } else {
                break;
            }
        }

        while traces.len() > policy.max_count {
            traces.pop_front();
        }

        Ok(before - traces.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_trace(id: &str, circuit: &str, started_at: u64) -> StoredTrace {
        StoredTrace {
            trace_id: id.to_string(),
            circuit: circuit.to_string(),
            status: "completed".to_string(),
            started_at,
            finished_at: started_at + 100,
            duration_ms: 100,
            outcome_type: Some("Next".to_string()),
            node_count: 3,
            fault_count: 0,
            timeline_json: None,
        }
    }

    #[tokio::test]
    async fn ring_buffer_evicts_oldest_when_over_capacity() {
        let store = InMemoryTraceStore::with_ttl(3, 0); // TTL disabled

        for i in 0..5 {
            let trace = make_trace(&format!("t-{i}"), "TestCircuit", 1000 + i * 100);
            store.save(trace).await.unwrap();
        }

        let count = store.count().await.unwrap();
        assert_eq!(count, 3, "should keep only max_count traces");

        // Oldest (t-0, t-1) should have been evicted
        assert!(store.get("t-0").await.unwrap().is_none());
        assert!(store.get("t-1").await.unwrap().is_none());
        assert!(store.get("t-2").await.unwrap().is_some());
        assert!(store.get("t-3").await.unwrap().is_some());
        assert!(store.get("t-4").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn ttl_prunes_expired_traces_on_save() {
        // TTL of 1 second
        let store = InMemoryTraceStore::with_ttl(100, 1);

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        // Insert a trace from 2 seconds ago (should be expired)
        let old_trace = make_trace("old", "OldCircuit", now_ms - 2000);
        store.save(old_trace).await.unwrap();
        assert_eq!(store.count().await.unwrap(), 1);

        // Insert a new trace — the old one should be pruned
        let new_trace = make_trace("new", "NewCircuit", now_ms);
        store.save(new_trace).await.unwrap();

        assert_eq!(store.count().await.unwrap(), 1);
        assert!(store.get("old").await.unwrap().is_none());
        assert!(store.get("new").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn apply_retention_prunes_by_age_and_count() {
        let store = InMemoryTraceStore::new(1000);

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        // Insert traces: 3 old (10 seconds ago) and 2 recent
        for i in 0..3 {
            store
                .save(make_trace(&format!("old-{i}"), "C", now_ms - 10_000 + i))
                .await
                .unwrap();
        }
        for i in 0..2 {
            store
                .save(make_trace(&format!("new-{i}"), "C", now_ms + i))
                .await
                .unwrap();
        }
        assert_eq!(store.count().await.unwrap(), 5);

        let policy = RetentionPolicy {
            max_count: 100,
            max_age_secs: 5, // 5 seconds — old traces should be pruned
        };
        let pruned = store.apply_retention(&policy).await.unwrap();
        assert_eq!(pruned, 3);
        assert_eq!(store.count().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn query_filters_by_circuit_and_status() {
        let store = InMemoryTraceStore::with_ttl(100, 0); // TTL disabled

        let now_ms = 100_000u64;
        store.save(make_trace("t1", "Auth", now_ms)).await.unwrap();
        let mut fault_trace = make_trace("t2", "Order", now_ms + 1);
        fault_trace.status = "faulted".to_string();
        store.save(fault_trace).await.unwrap();
        store
            .save(make_trace("t3", "Auth", now_ms + 2))
            .await
            .unwrap();

        let auth_only = store
            .query(TraceQuery {
                circuit: Some("Auth".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(auth_only.len(), 2);

        let faulted = store
            .query(TraceQuery {
                status: Some("faulted".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(faulted.len(), 1);
        assert_eq!(faulted[0].trace_id, "t2");
    }
}
