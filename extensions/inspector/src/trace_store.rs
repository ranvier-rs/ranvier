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
pub struct InMemoryTraceStore {
    traces: std::sync::Mutex<Vec<StoredTrace>>,
    max_count: usize,
}

impl InMemoryTraceStore {
    pub fn new(max_count: usize) -> Self {
        Self {
            traces: std::sync::Mutex::new(Vec::new()),
            max_count,
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
        traces.push(trace);
        // Trim from the front if over capacity
        while traces.len() > self.max_count {
            traces.remove(0);
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

        traces.retain(|t| t.started_at >= cutoff);

        while traces.len() > policy.max_count {
            traces.remove(0);
        }

        Ok(before - traces.len())
    }
}
