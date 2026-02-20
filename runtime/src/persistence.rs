use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistenceEnvelope {
    pub trace_id: String,
    pub circuit: String,
    pub step: u64,
    pub outcome_kind: String,
    pub timestamp_ms: u64,
    pub payload_hash: Option<String>,
}

/// Final completion state tracked for a persisted trace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompletionState {
    Success,
    Fault,
    Cancelled,
    Compensated,
}

#[cfg(feature = "persistence-postgres")]
fn completion_state_to_wire(state: &CompletionState) -> &'static str {
    match state {
        CompletionState::Success => "success",
        CompletionState::Fault => "fault",
        CompletionState::Cancelled => "cancelled",
        CompletionState::Compensated => "compensated",
    }
}

#[cfg(feature = "persistence-postgres")]
fn completion_state_from_wire(value: &str) -> Result<CompletionState> {
    match value {
        "success" => Ok(CompletionState::Success),
        "fault" => Ok(CompletionState::Fault),
        "cancelled" => Ok(CompletionState::Cancelled),
        "compensated" => Ok(CompletionState::Compensated),
        other => Err(anyhow!("unknown completion state value: {}", other)),
    }
}

/// Stored trace state returned from [`PersistenceStore::load`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedTrace {
    pub trace_id: String,
    pub circuit: String,
    pub events: Vec<PersistenceEnvelope>,
    pub resumed_from_step: Option<u64>,
    pub completion: Option<CompletionState>,
}

/// Resume cursor returned from [`PersistenceStore::resume`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeCursor {
    pub trace_id: String,
    pub next_step: u64,
}

/// Optional trace identifier override for persistence hooks.
///
/// Insert into `Bus` when a stable trace identity is required across process restarts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistenceTraceId(pub String);

impl PersistenceTraceId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

/// Controls whether runtime execution should call `complete` automatically.
///
/// Default runtime behavior when this resource is absent: `true`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersistenceAutoComplete(pub bool);

/// Runtime context delivered to compensation hooks.
///
/// The context is intentionally compact so hooks can map it to idempotent
/// compensating actions in domain/infrastructure layers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompensationContext {
    pub trace_id: String,
    pub circuit: String,
    pub fault_kind: String,
    pub fault_step: u64,
    pub timestamp_ms: u64,
}

/// Controls whether compensation hooks should run automatically on `Fault`.
///
/// Default runtime behavior when this resource is absent: `true`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompensationAutoTrigger(pub bool);

/// Compensation hook contract for irreversible side effects.
#[async_trait]
pub trait CompensationHook: Send + Sync {
    async fn compensate(&self, context: CompensationContext) -> Result<()>;
}

/// Bus-insertable compensation hook handle used by runtime execution hooks.
#[derive(Clone)]
pub struct CompensationHandle {
    inner: Arc<dyn CompensationHook>,
}

impl std::fmt::Debug for CompensationHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompensationHandle").finish_non_exhaustive()
    }
}

impl CompensationHandle {
    /// Create a handle from a concrete compensation hook implementation.
    pub fn from_hook<H>(hook: H) -> Self
    where
        H: CompensationHook + 'static,
    {
        Self {
            inner: Arc::new(hook),
        }
    }

    /// Create a handle from an existing trait-object Arc.
    pub fn from_arc(hook: Arc<dyn CompensationHook>) -> Self {
        Self { inner: hook }
    }

    /// Access the shared compensation hook.
    pub fn hook(&self) -> Arc<dyn CompensationHook> {
        self.inner.clone()
    }
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

/// Bus-insertable persistence handle used by runtime execution hooks.
#[derive(Clone)]
pub struct PersistenceHandle {
    inner: Arc<dyn PersistenceStore>,
}

impl std::fmt::Debug for PersistenceHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersistenceHandle").finish_non_exhaustive()
    }
}

impl PersistenceHandle {
    /// Create a handle from a concrete store implementation.
    pub fn from_store<S>(store: S) -> Self
    where
        S: PersistenceStore + 'static,
    {
        Self {
            inner: Arc::new(store),
        }
    }

    /// Create a handle from an existing trait-object Arc.
    pub fn from_arc(store: Arc<dyn PersistenceStore>) -> Self {
        Self { inner: store }
    }

    /// Access the shared persistence store.
    pub fn store(&self) -> Arc<dyn PersistenceStore> {
        self.inner.clone()
    }
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

#[cfg(feature = "persistence-postgres")]
#[derive(Debug, Clone)]
pub struct PostgresPersistenceStore {
    pool: sqlx::Pool<sqlx::Postgres>,
    events_table: String,
    state_table: String,
}

#[cfg(feature = "persistence-postgres")]
#[derive(sqlx::FromRow)]
struct PostgresEventRow {
    trace_id: String,
    circuit: String,
    step: i64,
    outcome_kind: String,
    timestamp_ms: i64,
    payload_hash: Option<String>,
}

#[cfg(feature = "persistence-postgres")]
#[derive(sqlx::FromRow)]
struct PostgresStateRow {
    trace_id: String,
    circuit: String,
    resumed_from_step: Option<i64>,
    completion: Option<String>,
}

#[cfg(feature = "persistence-postgres")]
impl PostgresPersistenceStore {
    /// Create a PostgreSQL-backed persistence store.
    ///
    /// This is an alpha adapter intended for M148 validation.
    pub fn new(pool: sqlx::Pool<sqlx::Postgres>) -> Self {
        Self::with_table_prefix(pool, "ranvier_persistence")
    }

    /// Create with custom table prefix.
    pub fn with_table_prefix(pool: sqlx::Pool<sqlx::Postgres>, prefix: impl Into<String>) -> Self {
        let prefix = prefix.into();
        Self {
            pool,
            events_table: format!("{}_events", prefix),
            state_table: format!("{}_state", prefix),
        }
    }

    /// Initialize adapter tables when absent.
    pub async fn ensure_schema(&self) -> Result<()> {
        let create_state = format!(
            "CREATE TABLE IF NOT EXISTS {} (
                trace_id TEXT PRIMARY KEY,
                circuit TEXT NOT NULL,
                resumed_from_step BIGINT NULL,
                completion TEXT NULL
            )",
            self.state_table
        );
        sqlx::query(&create_state).execute(&self.pool).await?;

        let create_events = format!(
            "CREATE TABLE IF NOT EXISTS {} (
                trace_id TEXT NOT NULL,
                circuit TEXT NOT NULL,
                step BIGINT NOT NULL,
                outcome_kind TEXT NOT NULL,
                timestamp_ms BIGINT NOT NULL,
                payload_hash TEXT NULL,
                PRIMARY KEY (trace_id, step)
            )",
            self.events_table
        );
        sqlx::query(&create_events).execute(&self.pool).await?;
        Ok(())
    }
}

#[cfg(feature = "persistence-postgres")]
#[async_trait]
impl PersistenceStore for PostgresPersistenceStore {
    async fn append(&self, envelope: PersistenceEnvelope) -> Result<()> {
        let insert_state = format!(
            "INSERT INTO {} (trace_id, circuit, resumed_from_step, completion)
             VALUES ($1, $2, NULL, NULL)
             ON CONFLICT (trace_id) DO NOTHING",
            self.state_table
        );
        sqlx::query(&insert_state)
            .bind(&envelope.trace_id)
            .bind(&envelope.circuit)
            .execute(&self.pool)
            .await?;

        let read_state = format!("SELECT circuit FROM {} WHERE trace_id = $1", self.state_table);
        let existing_circuit: Option<String> = sqlx::query_scalar(&read_state)
            .bind(&envelope.trace_id)
            .fetch_optional(&self.pool)
            .await?;
        if existing_circuit.as_deref() != Some(envelope.circuit.as_str()) {
            return Err(anyhow!(
                "trace_id {} already exists for another circuit",
                envelope.trace_id
            ));
        }

        let completion_query = format!("SELECT completion FROM {} WHERE trace_id = $1", self.state_table);
        let completion: Option<String> = sqlx::query_scalar(&completion_query)
            .bind(&envelope.trace_id)
            .fetch_optional(&self.pool)
            .await?;
        if completion.is_some() {
            return Err(anyhow!(
                "trace_id {} is already completed and cannot accept new events",
                envelope.trace_id
            ));
        }

        let step_i64 = i64::try_from(envelope.step)?;
        let ts_i64 = i64::try_from(envelope.timestamp_ms)?;
        let insert_event = format!(
            "INSERT INTO {} (trace_id, circuit, step, outcome_kind, timestamp_ms, payload_hash)
             VALUES ($1, $2, $3, $4, $5, $6)",
            self.events_table
        );
        sqlx::query(&insert_event)
            .bind(&envelope.trace_id)
            .bind(&envelope.circuit)
            .bind(step_i64)
            .bind(&envelope.outcome_kind)
            .bind(ts_i64)
            .bind(&envelope.payload_hash)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn load(&self, trace_id: &str) -> Result<Option<PersistedTrace>> {
        let state_query = format!(
            "SELECT trace_id, circuit, resumed_from_step, completion
             FROM {}
             WHERE trace_id = $1",
            self.state_table
        );
        let Some(state): Option<PostgresStateRow> = sqlx::query_as(&state_query)
            .bind(trace_id)
            .fetch_optional(&self.pool)
            .await?
        else {
            return Ok(None);
        };

        let events_query = format!(
            "SELECT trace_id, circuit, step, outcome_kind, timestamp_ms, payload_hash
             FROM {}
             WHERE trace_id = $1
             ORDER BY step ASC",
            self.events_table
        );
        let rows: Vec<PostgresEventRow> = sqlx::query_as(&events_query)
            .bind(trace_id)
            .fetch_all(&self.pool)
            .await?;

        let mut events = Vec::with_capacity(rows.len());
        for row in rows {
            events.push(PersistenceEnvelope {
                trace_id: row.trace_id,
                circuit: row.circuit,
                step: u64::try_from(row.step)?,
                outcome_kind: row.outcome_kind,
                timestamp_ms: u64::try_from(row.timestamp_ms)?,
                payload_hash: row.payload_hash,
            });
        }

        let completion = match state.completion {
            Some(value) => Some(completion_state_from_wire(&value)?),
            None => None,
        };

        Ok(Some(PersistedTrace {
            trace_id: state.trace_id,
            circuit: state.circuit,
            events,
            resumed_from_step: state.resumed_from_step.map(u64::try_from).transpose()?,
            completion,
        }))
    }

    async fn resume(&self, trace_id: &str, resume_from_step: u64) -> Result<ResumeCursor> {
        let query = format!(
            "UPDATE {}
             SET resumed_from_step = $2
             WHERE trace_id = $1",
            self.state_table
        );
        let rows = sqlx::query(&query)
            .bind(trace_id)
            .bind(i64::try_from(resume_from_step)?)
            .execute(&self.pool)
            .await?
            .rows_affected();
        if rows == 0 {
            return Err(anyhow!("trace_id {} not found", trace_id));
        }
        Ok(ResumeCursor {
            trace_id: trace_id.to_string(),
            next_step: resume_from_step.saturating_add(1),
        })
    }

    async fn complete(&self, trace_id: &str, completion: CompletionState) -> Result<()> {
        let query = format!(
            "UPDATE {}
             SET completion = $2
             WHERE trace_id = $1",
            self.state_table
        );
        let rows = sqlx::query(&query)
            .bind(trace_id)
            .bind(completion_state_to_wire(&completion))
            .execute(&self.pool)
            .await?
            .rows_affected();
        if rows == 0 {
            return Err(anyhow!("trace_id {} not found", trace_id));
        }
        Ok(())
    }
}

#[cfg(feature = "persistence-redis")]
#[derive(Clone)]
pub struct RedisPersistenceStore {
    manager: redis::aio::ConnectionManager,
    key_prefix: String,
}

#[cfg(feature = "persistence-redis")]
impl std::fmt::Debug for RedisPersistenceStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisPersistenceStore")
            .field("key_prefix", &self.key_prefix)
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "persistence-redis")]
impl RedisPersistenceStore {
    /// Connect using Redis connection URL.
    ///
    /// Example: `redis://127.0.0.1:6379`
    pub async fn connect(url: &str) -> Result<Self> {
        let client = redis::Client::open(url)?;
        let manager = redis::aio::ConnectionManager::new(client).await?;
        Ok(Self {
            manager,
            key_prefix: "ranvier:persistence".to_string(),
        })
    }

    pub fn with_prefix(manager: redis::aio::ConnectionManager, key_prefix: impl Into<String>) -> Self {
        Self {
            manager,
            key_prefix: key_prefix.into(),
        }
    }

    fn key(&self, trace_id: &str) -> String {
        format!("{}:{}", self.key_prefix, trace_id)
    }

    async fn write_trace(&self, trace: &PersistedTrace) -> Result<()> {
        use redis::AsyncCommands;
        let key = self.key(&trace.trace_id);
        let payload = serde_json::to_string(trace)?;
        let mut conn = self.manager.clone();
        conn.set::<_, _, ()>(key, payload).await?;
        Ok(())
    }
}

#[cfg(feature = "persistence-redis")]
#[async_trait]
impl PersistenceStore for RedisPersistenceStore {
    async fn append(&self, envelope: PersistenceEnvelope) -> Result<()> {
        let mut trace = self
            .load(&envelope.trace_id)
            .await?
            .unwrap_or_else(|| PersistedTrace {
                trace_id: envelope.trace_id.clone(),
                circuit: envelope.circuit.clone(),
                events: Vec::new(),
                resumed_from_step: None,
                completion: None,
            });

        if trace.circuit != envelope.circuit {
            return Err(anyhow!(
                "trace_id {} already exists for circuit {}, got {}",
                envelope.trace_id,
                trace.circuit,
                envelope.circuit
            ));
        }
        if trace.completion.is_some() {
            return Err(anyhow!(
                "trace_id {} is already completed and cannot accept new events",
                envelope.trace_id
            ));
        }

        trace.events.push(envelope);
        trace.events.sort_by_key(|event| event.step);
        self.write_trace(&trace).await?;
        Ok(())
    }

    async fn load(&self, trace_id: &str) -> Result<Option<PersistedTrace>> {
        use redis::AsyncCommands;
        let key = self.key(trace_id);
        let mut conn = self.manager.clone();
        let payload: Option<String> = conn.get(key).await?;
        let trace = payload.map(|raw| serde_json::from_str::<PersistedTrace>(&raw)).transpose()?;
        Ok(trace)
    }

    async fn resume(&self, trace_id: &str, resume_from_step: u64) -> Result<ResumeCursor> {
        let mut trace = self
            .load(trace_id)
            .await?
            .ok_or_else(|| anyhow!("trace_id {} not found", trace_id))?;
        trace.resumed_from_step = Some(resume_from_step);
        self.write_trace(&trace).await?;
        Ok(ResumeCursor {
            trace_id: trace_id.to_string(),
            next_step: resume_from_step.saturating_add(1),
        })
    }

    async fn complete(&self, trace_id: &str, completion: CompletionState) -> Result<()> {
        let mut trace = self
            .load(trace_id)
            .await?
            .ok_or_else(|| anyhow!("trace_id {} not found", trace_id))?;
        trace.completion = Some(completion);
        self.write_trace(&trace).await?;
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
