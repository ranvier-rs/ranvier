use anyhow::{anyhow, Result};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Minimal persisted envelope for Axon execution checkpoints.
///
/// M148 baseline contract fields:
/// - trace
/// - circuit
/// - step
/// - outcome
/// - timestamp
/// - payload hash
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistenceEnvelope {
    pub trace_id: String,
    pub circuit: String,
    pub step: u64,
    pub outcome_kind: String,
    pub timestamp_ms: u64,
    pub payload_hash: Option<String>,
}

/// Final completion state tracked for a persisted trace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionState {
    Success,
    Fault,
    Cancelled,
    Compensated,
}

/// Stored trace state returned from [`PersistenceStore::load`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedTrace {
    pub trace_id: String,
    pub circuit: String,
    pub events: Vec<PersistenceEnvelope>,
    pub resumed_from_step: Option<u64>,
    pub completion: Option<CompletionState>,
}

/// Resume cursor returned from [`PersistenceStore::resume`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeCursor {
    pub trace_id: String,
    pub next_step: u64,
}

/// Persistence abstraction draft for long-running workflow recovery.
///
/// This is intentionally minimal and marked experimental while M148 is active.
#[async_trait]
pub trait PersistenceStore: Send + Sync {
    async fn append(&self, envelope: PersistenceEnvelope) -> Result<()>;
    async fn load(&self, trace_id: &str) -> Result<Option<PersistedTrace>>;
    async fn resume(&self, trace_id: &str, resume_from_step: u64) -> Result<ResumeCursor>;
    async fn complete(&self, trace_id: &str, completion: CompletionState) -> Result<()>;
}

/// In-memory reference adapter for local testing and contract validation.
#[derive(Debug, Default, Clone)]
pub struct InMemoryPersistenceStore {
    inner: Arc<RwLock<HashMap<String, PersistedTrace>>>,
}

impl InMemoryPersistenceStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl PersistenceStore for InMemoryPersistenceStore {
    async fn append(&self, envelope: PersistenceEnvelope) -> Result<()> {
        let mut guard = self.inner.write().await;
        let entry = guard
            .entry(envelope.trace_id.clone())
            .or_insert_with(|| PersistedTrace {
                trace_id: envelope.trace_id.clone(),
                circuit: envelope.circuit.clone(),
                events: Vec::new(),
                resumed_from_step: None,
                completion: None,
            });

        if entry.circuit != envelope.circuit {
            return Err(anyhow!(
                "trace_id {} already exists for circuit {}, got {}",
                envelope.trace_id,
                entry.circuit,
                envelope.circuit
            ));
        }
        if entry.completion.is_some() {
            return Err(anyhow!(
                "trace_id {} is already completed and cannot accept new events",
                envelope.trace_id
            ));
        }
        entry.events.push(envelope);
        entry.events.sort_by_key(|e| e.step);
        Ok(())
    }

    async fn load(&self, trace_id: &str) -> Result<Option<PersistedTrace>> {
        let guard = self.inner.read().await;
        Ok(guard.get(trace_id).cloned())
    }

    async fn resume(&self, trace_id: &str, resume_from_step: u64) -> Result<ResumeCursor> {
        let mut guard = self.inner.write().await;
        let trace = guard
            .get_mut(trace_id)
            .ok_or_else(|| anyhow!("trace_id {} not found", trace_id))?;
        trace.resumed_from_step = Some(resume_from_step);
        Ok(ResumeCursor {
            trace_id: trace_id.to_string(),
            next_step: resume_from_step.saturating_add(1),
        })
    }

    async fn complete(&self, trace_id: &str, completion: CompletionState) -> Result<()> {
        let mut guard = self.inner.write().await;
        let trace = guard
            .get_mut(trace_id)
            .ok_or_else(|| anyhow!("trace_id {} not found", trace_id))?;
        trace.completion = Some(completion);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(step: u64, outcome_kind: &str) -> PersistenceEnvelope {
        PersistenceEnvelope {
            trace_id: "trace-1".to_string(),
            circuit: "OrderCircuit".to_string(),
            step,
            outcome_kind: outcome_kind.to_string(),
            timestamp_ms: 1_700_000_000_000 + step,
            payload_hash: Some(format!("hash-{}", step)),
        }
    }

    #[tokio::test]
    async fn append_and_load_roundtrip() {
        let store = InMemoryPersistenceStore::new();
        store.append(envelope(1, "Next")).await.unwrap();
        store.append(envelope(2, "Branch")).await.unwrap();

        let loaded = store.load("trace-1").await.unwrap().unwrap();
        assert_eq!(loaded.trace_id, "trace-1");
        assert_eq!(loaded.circuit, "OrderCircuit");
        assert_eq!(loaded.events.len(), 2);
        assert_eq!(loaded.events[0].step, 1);
        assert_eq!(loaded.events[1].outcome_kind, "Branch");
        assert_eq!(loaded.completion, None);
    }

    #[tokio::test]
    async fn resume_records_cursor() {
        let store = InMemoryPersistenceStore::new();
        store.append(envelope(3, "Fault")).await.unwrap();

        let cursor = store.resume("trace-1", 3).await.unwrap();
        assert_eq!(
            cursor,
            ResumeCursor {
                trace_id: "trace-1".to_string(),
                next_step: 4
            }
        );

        let loaded = store.load("trace-1").await.unwrap().unwrap();
        assert_eq!(loaded.resumed_from_step, Some(3));
    }

    #[tokio::test]
    async fn complete_marks_trace_and_blocks_append() {
        let store = InMemoryPersistenceStore::new();
        store.append(envelope(1, "Next")).await.unwrap();
        store
            .complete("trace-1", CompletionState::Success)
            .await
            .unwrap();

        let loaded = store.load("trace-1").await.unwrap().unwrap();
        assert_eq!(loaded.completion, Some(CompletionState::Success));

        let err = store.append(envelope(2, "Next")).await.unwrap_err();
        assert!(err
            .to_string()
            .contains("is already completed and cannot accept new events"));
    }

    #[tokio::test]
    async fn append_rejects_cross_circuit_trace_reuse() {
        let store = InMemoryPersistenceStore::new();
        store.append(envelope(1, "Next")).await.unwrap();

        let mut invalid = envelope(2, "Next");
        invalid.circuit = "AnotherCircuit".to_string();
        let err = store.append(invalid).await.unwrap_err();
        assert!(err
            .to_string()
            .contains("already exists for circuit OrderCircuit"));
    }
}
