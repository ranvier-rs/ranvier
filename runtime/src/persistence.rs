use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
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
    pub schematic_version: String,
    pub step: u64,
    pub node_id: Option<String>,
    pub outcome_kind: String,
    pub timestamp_ms: u64,
    pub payload_hash: Option<String>,
    pub payload: Option<serde_json::Value>,
}

/// Manual intervention command for a trace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intervention {
    pub target_node: String,
    pub payload_override: Option<serde_json::Value>,
    pub timestamp_ms: u64,
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
    /// The schematic version of the last appended event.
    pub schematic_version: String,
    pub events: Vec<PersistenceEnvelope>,
    pub interventions: Vec<Intervention>,
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

/// Retry policy for compensation hook execution.
///
/// Defaults to a single attempt (no retry).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompensationRetryPolicy {
    pub max_attempts: u32,
    pub backoff_ms: u64,
}

impl Default for CompensationRetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 1,
            backoff_ms: 0,
        }
    }
}

/// Compensation hook contract for irreversible side effects.
///
/// **Production-stable since 0.12 (M156).** Implement and register this hook
/// via [`CompensationHandle`] to define rollback actions when an Axon pipeline
/// faults after committing external side effects.
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

/// Idempotency store contract for compensation execution deduplication.
///
/// **Production-stable since 0.12 (M156).** Ensures that compensation actions
/// are executed at most once even when retried after a process restart.
/// Built-in adapters: [`InMemoryCompensationIdempotencyStore`],
/// [`PostgresCompensationIdempotencyStore`] (feature `persistence-postgres`),
/// [`RedisCompensationIdempotencyStore`] (feature `persistence-redis`).
#[async_trait]
pub trait CompensationIdempotencyStore: Send + Sync {
    async fn was_compensated(&self, key: &str) -> Result<bool>;
    async fn mark_compensated(&self, key: &str) -> Result<()>;
}

/// Bus-insertable idempotency handle for compensation hooks.
#[derive(Clone)]
pub struct CompensationIdempotencyHandle {
    inner: Arc<dyn CompensationIdempotencyStore>,
}

impl std::fmt::Debug for CompensationIdempotencyHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompensationIdempotencyHandle")
            .finish_non_exhaustive()
    }
}

impl CompensationIdempotencyHandle {
    pub fn from_store<S>(store: S) -> Self
    where
        S: CompensationIdempotencyStore + 'static,
    {
        Self {
            inner: Arc::new(store),
        }
    }

    pub fn from_arc(store: Arc<dyn CompensationIdempotencyStore>) -> Self {
        Self { inner: store }
    }

    pub fn store(&self) -> Arc<dyn CompensationIdempotencyStore> {
        self.inner.clone()
    }
}

/// In-memory idempotency store for compensation deduplication.
#[derive(Debug, Default, Clone)]
pub struct InMemoryCompensationIdempotencyStore {
    keys: Arc<RwLock<HashSet<String>>>,
}

impl InMemoryCompensationIdempotencyStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl CompensationIdempotencyStore for InMemoryCompensationIdempotencyStore {
    async fn was_compensated(&self, key: &str) -> Result<bool> {
        let guard = self.keys.read().await;
        Ok(guard.contains(key))
    }

    async fn mark_compensated(&self, key: &str) -> Result<()> {
        let mut guard = self.keys.write().await;
        guard.insert(key.to_string());
        Ok(())
    }
}

#[cfg(feature = "persistence-postgres")]
#[derive(Debug, Clone)]
pub struct PostgresCompensationIdempotencyStore {
    pool: sqlx::Pool<sqlx::Postgres>,
    table: String,
}

#[cfg(feature = "persistence-postgres")]
impl PostgresCompensationIdempotencyStore {
    /// Create a PostgreSQL-backed compensation idempotency store.
    pub fn new(pool: sqlx::Pool<sqlx::Postgres>) -> Self {
        Self::with_table_prefix(pool, "ranvier_persistence")
    }

    /// Create with custom table prefix.
    pub fn with_table_prefix(pool: sqlx::Pool<sqlx::Postgres>, prefix: impl Into<String>) -> Self {
        let prefix = prefix.into();
        Self {
            pool,
            table: format!("{}_compensation_idempotency", prefix),
        }
    }

    /// Initialize adapter table when absent.
    pub async fn ensure_schema(&self) -> Result<()> {
        let create = format!(
            "CREATE TABLE IF NOT EXISTS {} (
                idempotency_key TEXT PRIMARY KEY,
                created_at_ms BIGINT NOT NULL
            )",
            self.table
        );
        sqlx::query(&create).execute(&self.pool).await?;
        Ok(())
    }

    /// Remove stale idempotency rows older than `cutoff_ms` (unix epoch ms).
    pub async fn purge_older_than_ms(&self, cutoff_ms: i64) -> Result<u64> {
        let query = format!(
            "DELETE FROM {}
             WHERE created_at_ms < $1",
            self.table
        );
        let rows = sqlx::query(&query)
            .bind(cutoff_ms)
            .execute(&self.pool)
            .await?
            .rows_affected();
        Ok(rows)
    }
}

#[cfg(feature = "persistence-postgres")]
#[async_trait]
impl CompensationIdempotencyStore for PostgresCompensationIdempotencyStore {
    async fn was_compensated(&self, key: &str) -> Result<bool> {
        let query = format!(
            "SELECT 1
             FROM {}
             WHERE idempotency_key = $1
             LIMIT 1",
            self.table
        );
        let row: Option<i32> = sqlx::query_scalar(&query)
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some())
    }

    async fn mark_compensated(&self, key: &str) -> Result<()> {
        let query = format!(
            "INSERT INTO {} (idempotency_key, created_at_ms)
             VALUES ($1, $2)
             ON CONFLICT (idempotency_key) DO NOTHING",
            self.table
        );
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis();
        sqlx::query(&query)
            .bind(key)
            .bind(i64::try_from(now_ms)?)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[cfg(feature = "persistence-redis")]
#[derive(Clone)]
pub struct RedisCompensationIdempotencyStore {
    manager: redis::aio::ConnectionManager,
    key_prefix: String,
    ttl_seconds: Option<u64>,
}

#[cfg(feature = "persistence-redis")]
impl std::fmt::Debug for RedisCompensationIdempotencyStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisCompensationIdempotencyStore")
            .field("key_prefix", &self.key_prefix)
            .field("ttl_seconds", &self.ttl_seconds)
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "persistence-redis")]
impl RedisCompensationIdempotencyStore {
    /// Connect using Redis connection URL.
    pub async fn connect(url: &str) -> Result<Self> {
        let client = redis::Client::open(url)?;
        let manager = redis::aio::ConnectionManager::new(client).await?;
        Ok(Self {
            manager,
            key_prefix: "ranvier:compensation:idempotency".to_string(),
            ttl_seconds: None,
        })
    }

    pub fn with_prefix(
        manager: redis::aio::ConnectionManager,
        key_prefix: impl Into<String>,
    ) -> Self {
        Self {
            manager,
            key_prefix: key_prefix.into(),
            ttl_seconds: None,
        }
    }

    pub fn with_prefix_and_ttl(
        manager: redis::aio::ConnectionManager,
        key_prefix: impl Into<String>,
        ttl_seconds: u64,
    ) -> Self {
        Self {
            manager,
            key_prefix: key_prefix.into(),
            ttl_seconds: Some(ttl_seconds),
        }
    }

    fn key(&self, idempotency_key: &str) -> String {
        format!("{}:{}", self.key_prefix, idempotency_key)
    }
}

#[cfg(feature = "persistence-redis")]
#[async_trait]
impl CompensationIdempotencyStore for RedisCompensationIdempotencyStore {
    async fn was_compensated(&self, key: &str) -> Result<bool> {
        use redis::AsyncCommands;
        let mut conn = self.manager.clone();
        let exists: bool = conn.exists(self.key(key)).await?;
        Ok(exists)
    }

    async fn mark_compensated(&self, key: &str) -> Result<()> {
        use redis::AsyncCommands;
        let mut conn = self.manager.clone();
        let redis_key = self.key(key);
        let inserted: bool = conn.set_nx(&redis_key, "1").await?;
        if inserted {
            if let Some(ttl_seconds) = self.ttl_seconds {
                let ttl_i64 = i64::try_from(ttl_seconds)?;
                let _: bool = conn.expire(&redis_key, ttl_i64).await?;
            }
        }
        Ok(())
    }
}

/// Persistence abstraction for long-running workflow crash recovery.
///
/// **Production-stable since 0.12 (M156).**
///
/// Implement this trait to store and reload Axon execution checkpoints across
/// process restarts. The runtime calls `append` on each step and `complete` on
/// terminal outcomes. Use `resume` to replay a pipeline from a saved cursor.
///
/// # Built-in adapters
///
/// | Adapter | Feature flag | Best for |
/// |---|---|---|
/// | [`InMemoryPersistenceStore`] | none (default) | tests, local dev |
/// | [`PostgresPersistenceStore`] | `persistence-postgres` | durable prod storage |
/// | [`RedisPersistenceStore`] | `persistence-redis` | ephemeral/fast checkpoints |
///
/// # See also
///
/// - `docs/03_guides/persistence_ops_runbook.md` — operational playbook
/// - `docs/manual/04_PERSISTENCE.md` — concept guide + adapter selection
#[async_trait]
pub trait PersistenceStore: Send + Sync {
    async fn append(&self, envelope: PersistenceEnvelope) -> Result<()>;
    async fn load(&self, trace_id: &str) -> Result<Option<PersistedTrace>>;
    async fn resume(&self, trace_id: &str, resume_from_step: u64) -> Result<ResumeCursor>;
    async fn complete(&self, trace_id: &str, completion: CompletionState) -> Result<()>;
    async fn save_intervention(&self, trace_id: &str, intervention: Intervention) -> Result<()>;
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
                schematic_version: envelope.schematic_version.clone(),
                events: Vec::new(),
                interventions: Vec::new(),
                resumed_from_step: None,
                completion: None,
            });

        entry.schematic_version = envelope.schematic_version.clone();

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

    async fn save_intervention(&self, trace_id: &str, intervention: Intervention) -> Result<()> {
        let mut guard = self.inner.write().await;
        let trace = guard
            .get_mut(trace_id)
            .ok_or_else(|| anyhow!("trace_id {} not found", trace_id))?;
        
        // If trace is completed, we might want to "un-complete" it for resumption if it's a force resume
        // For now, just add the intervention.
        trace.interventions.push(intervention);
        Ok(())
    }
}

#[cfg(feature = "persistence-postgres")]
#[derive(Debug, Clone)]
pub struct PostgresPersistenceStore {
    pool: sqlx::Pool<sqlx::Postgres>,
    events_table: String,
    state_table: String,
    interventions_table: String,
}

#[cfg(feature = "persistence-postgres")]
#[derive(sqlx::FromRow)]
struct PostgresEventRow {
    trace_id: String,
    circuit: String,
    schematic_version: String,
    step: i64,
    outcome_kind: String,
    timestamp_ms: i64,
    payload_hash: Option<String>,
    payload: Option<serde_json::Value>,
}

#[cfg(feature = "persistence-postgres")]
#[derive(sqlx::FromRow)]
struct PostgresStateRow {
    trace_id: String,
    circuit: String,
    schematic_version: String,
    resumed_from_step: Option<i64>,
    completion: Option<String>,
}

#[cfg(feature = "persistence-postgres")]
#[derive(sqlx::FromRow)]
struct PostgresInterventionRow {
    trace_id: String,
    target_node: String,
    payload_override: Option<serde_json::Value>,
    timestamp_ms: i64,
}

#[cfg(feature = "persistence-postgres")]
impl PostgresPersistenceStore {
    /// Create a PostgreSQL-backed persistence store.
    ///
    /// Uses the default table prefix `ranvier_persistence`. Call
    /// `ensure_schema()` once at startup to create required tables.
    /// See [`Self::with_table_prefix`] for multi-tenant setups.
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
            interventions_table: format!("{}_interventions", prefix),
        }
    }

    /// Initialize adapter tables when absent.
    pub async fn ensure_schema(&self) -> Result<()> {
        let create_state = format!(
            "CREATE TABLE IF NOT EXISTS {} (
                trace_id TEXT PRIMARY KEY,
                circuit TEXT NOT NULL,
                schematic_version TEXT NOT NULL,
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
                schematic_version TEXT NOT NULL,
                step BIGINT NOT NULL,
                outcome_kind TEXT NOT NULL,
                timestamp_ms BIGINT NOT NULL,
                payload_hash TEXT NULL,
                payload JSONB NULL,
                PRIMARY KEY (trace_id, step)
            )",
            self.events_table
        );
        sqlx::query(&create_events).execute(&self.pool).await?;

        let create_interventions = format!(
            "CREATE TABLE IF NOT EXISTS {} (
                trace_id TEXT NOT NULL,
                target_node TEXT NOT NULL,
                payload_override JSONB NULL,
                timestamp_ms BIGINT NOT NULL,
                FOREIGN KEY (trace_id) REFERENCES {} (trace_id)
            )",
            self.interventions_table,
            self.state_table
        );
        sqlx::query(&create_interventions).execute(&self.pool).await?;

        Ok(())
    }
}

#[cfg(feature = "persistence-postgres")]
#[async_trait]
impl PersistenceStore for PostgresPersistenceStore {
    async fn append(&self, envelope: PersistenceEnvelope) -> Result<()> {
        let insert_state = format!(
            "INSERT INTO {} (trace_id, circuit, schematic_version, resumed_from_step, completion)
             VALUES ($1, $2, $3, NULL, NULL)
             ON CONFLICT (trace_id) DO UPDATE SET schematic_version = $3",
            self.state_table
        );
        sqlx::query(&insert_state)
            .bind(&envelope.trace_id)
            .bind(&envelope.circuit)
            .bind(&envelope.schematic_version)
            .execute(&self.pool)
            .await?;

        let read_state = format!(
            "SELECT circuit FROM {} WHERE trace_id = $1",
            self.state_table
        );
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

        let completion_query = format!(
            "SELECT completion FROM {} WHERE trace_id = $1",
            self.state_table
        );
        let completion: Option<Option<String>> = sqlx::query_scalar(&completion_query)
            .bind(&envelope.trace_id)
            .fetch_optional(&self.pool)
            .await?;
        if completion.flatten().is_some() {
            return Err(anyhow!(
                "trace_id {} is already completed and cannot accept new events",
                envelope.trace_id
            ));
        }

        let step_i64 = i64::try_from(envelope.step)?;
        let ts_i64 = i64::try_from(envelope.timestamp_ms)?;
        let insert_event = format!(
            "INSERT INTO {} (trace_id, circuit, schematic_version, step, outcome_kind, timestamp_ms, payload_hash, payload)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            self.events_table
        );
        sqlx::query(&insert_event)
            .bind(&envelope.trace_id)
            .bind(&envelope.circuit)
            .bind(&envelope.schematic_version)
            .bind(step_i64)
            .bind(&envelope.outcome_kind)
            .bind(ts_i64)
            .bind(&envelope.payload_hash)
            .bind(&envelope.payload)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn load(&self, trace_id: &str) -> Result<Option<PersistedTrace>> {
        let state_query = format!(
            "SELECT trace_id, circuit, schematic_version, resumed_from_step, completion
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
            "SELECT trace_id, circuit, schematic_version, step, outcome_kind, timestamp_ms, payload_hash, payload
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
                schematic_version: row.schematic_version,
                step: u64::try_from(row.step)?,
                node_id: None,
                outcome_kind: row.outcome_kind,
                timestamp_ms: u64::try_from(row.timestamp_ms)?,
                payload_hash: row.payload_hash,
                payload: row.payload,
            });
        }

        let completion = match state.completion {
            Some(value) => Some(completion_state_from_wire(&value)?),
            None => None,
        };

        let interventions_query = format!(
            "SELECT trace_id, target_node, payload_override, timestamp_ms
             FROM {}
             WHERE trace_id = $1
             ORDER BY timestamp_ms ASC",
            self.interventions_table
        );
        let intervention_rows: Vec<PostgresInterventionRow> = sqlx::query_as(&interventions_query)
            .bind(trace_id)
            .fetch_all(&self.pool)
            .await?;

        let mut interventions = Vec::with_capacity(intervention_rows.len());
        for row in intervention_rows {
            interventions.push(Intervention {
                target_node: row.target_node,
                payload_override: row.payload_override,
                timestamp_ms: u64::try_from(row.timestamp_ms)?,
            });
        }

        Ok(Some(PersistedTrace {
            trace_id: state.trace_id,
            circuit: state.circuit,
            schematic_version: state.schematic_version,
            events,
            interventions,
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

    async fn save_intervention(&self, trace_id: &str, intervention: Intervention) -> Result<()> {
        let ts_i64 = i64::try_from(intervention.timestamp_ms)?;
        let query = format!(
            "INSERT INTO {} (trace_id, target_node, payload_override, timestamp_ms)
             VALUES ($1, $2, $3, $4)",
            self.interventions_table
        );
        sqlx::query(&query)
            .bind(trace_id)
            .bind(&intervention.target_node)
            .bind(&intervention.payload_override)
            .bind(ts_i64)
            .execute(&self.pool)
            .await?;
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

    pub fn with_prefix(
        manager: redis::aio::ConnectionManager,
        key_prefix: impl Into<String>,
    ) -> Self {
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
                schematic_version: envelope.schematic_version.clone(),
                events: Vec::new(),
                interventions: Vec::new(),
                resumed_from_step: None,
                completion: None,
            });

        trace.schematic_version = envelope.schematic_version.clone();

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
        let trace = payload
            .map(|raw| serde_json::from_str::<PersistedTrace>(&raw))
            .transpose()?;
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

    async fn save_intervention(&self, trace_id: &str, intervention: Intervention) -> Result<()> {
        let mut trace = self
            .load(trace_id)
            .await?
            .ok_or_else(|| anyhow!("trace_id {} not found", trace_id))?;
        trace.interventions.push(intervention);
        self.write_trace(&trace).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(any(feature = "persistence-postgres", feature = "persistence-redis"))]
    use uuid::Uuid;

    fn envelope(step: u64, outcome_kind: &str) -> PersistenceEnvelope {
        PersistenceEnvelope {
            trace_id: "trace-1".to_string(),
            circuit: "OrderCircuit".to_string(),
            schematic_version: "1.0".to_string(),
            step,
            node_id: None,
            outcome_kind: outcome_kind.to_string(),
            timestamp_ms: 1_700_000_000_000 + step,
            payload_hash: Some(format!("hash-{}", step)),
            payload: None,
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
        assert!(
            err.to_string()
                .contains("is already completed and cannot accept new events")
        );
    }

    #[tokio::test]
    async fn append_rejects_cross_circuit_trace_reuse() {
        let store = InMemoryPersistenceStore::new();
        store.append(envelope(1, "Next")).await.unwrap();

        let mut invalid = envelope(2, "Next");
        invalid.circuit = "AnotherCircuit".to_string();
        let err = store.append(invalid).await.unwrap_err();
        assert!(
            err.to_string()
                .contains("already exists for circuit OrderCircuit")
        );
    }

    #[tokio::test]
    async fn in_memory_compensation_idempotency_roundtrip() {
        let store = InMemoryCompensationIdempotencyStore::new();
        let key = "trace-a:OrderFlow:Fault";

        assert!(!store.was_compensated(key).await.unwrap());
        store.mark_compensated(key).await.unwrap();
        assert!(store.was_compensated(key).await.unwrap());
    }

    #[cfg(feature = "persistence-postgres")]
    #[tokio::test]
    async fn postgres_store_roundtrip_when_configured() {
        let url = match std::env::var("RANVIER_PERSISTENCE_POSTGRES_URL") {
            Ok(value) => value,
            Err(_) => return,
        };

        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .connect(&url)
            .await
            .unwrap();
        let table_prefix = format!("ranvier_persistence_test_{}", Uuid::new_v4().simple());
        let store = PostgresPersistenceStore::with_table_prefix(pool.clone(), table_prefix.clone());
        store.ensure_schema().await.unwrap();

        let trace_id = format!("trace-{}", Uuid::new_v4().simple());
        let circuit = "PgCircuit".to_string();

        let mut first = envelope(1, "Next");
        first.trace_id = trace_id.clone();
        first.circuit = circuit.clone();
        store.append(first).await.unwrap();

        let mut second = envelope(2, "Branch");
        second.trace_id = trace_id.clone();
        second.circuit = circuit.clone();
        store.append(second).await.unwrap();

        let cursor = store.resume(&trace_id, 2).await.unwrap();
        assert_eq!(cursor.next_step, 3);

        store
            .complete(&trace_id, CompletionState::Compensated)
            .await
            .unwrap();

        let loaded = store.load(&trace_id).await.unwrap().unwrap();
        assert_eq!(loaded.trace_id, trace_id);
        assert_eq!(loaded.circuit, circuit);
        assert_eq!(loaded.events.len(), 2);
        assert_eq!(loaded.resumed_from_step, Some(2));
        assert_eq!(loaded.completion, Some(CompletionState::Compensated));

        let drop_events = format!("DROP TABLE IF EXISTS {}", store.events_table);
        let drop_state = format!("DROP TABLE IF EXISTS {}", store.state_table);
        sqlx::query(&drop_events).execute(&pool).await.unwrap();
        sqlx::query(&drop_state).execute(&pool).await.unwrap();
    }

    #[cfg(feature = "persistence-redis")]
    #[tokio::test]
    async fn redis_store_roundtrip_when_configured() {
        let url = match std::env::var("RANVIER_PERSISTENCE_REDIS_URL") {
            Ok(value) => value,
            Err(_) => return,
        };

        let base = RedisPersistenceStore::connect(&url).await.unwrap();
        let prefix = format!("ranvier:persistence:test:{}", Uuid::new_v4().simple());
        let store = RedisPersistenceStore::with_prefix(base.manager.clone(), prefix);

        let trace_id = format!("trace-{}", Uuid::new_v4().simple());
        let circuit = "RedisCircuit".to_string();

        let mut first = envelope(1, "Next");
        first.trace_id = trace_id.clone();
        first.circuit = circuit.clone();
        store.append(first).await.unwrap();

        let mut second = envelope(2, "Fault");
        second.trace_id = trace_id.clone();
        second.circuit = circuit.clone();
        store.append(second).await.unwrap();

        let cursor = store.resume(&trace_id, 2).await.unwrap();
        assert_eq!(cursor.next_step, 3);

        store
            .complete(&trace_id, CompletionState::Fault)
            .await
            .unwrap();

        let loaded = store.load(&trace_id).await.unwrap().unwrap();
        assert_eq!(loaded.trace_id, trace_id);
        assert_eq!(loaded.circuit, circuit);
        assert_eq!(loaded.events.len(), 2);
        assert_eq!(loaded.resumed_from_step, Some(2));
        assert_eq!(loaded.completion, Some(CompletionState::Fault));

        use redis::AsyncCommands;
        let key = store.key(&trace_id);
        let mut conn = store.manager.clone();
        let _: () = conn.del(key).await.unwrap();
    }

    #[cfg(feature = "persistence-postgres")]
    #[tokio::test]
    async fn postgres_compensation_idempotency_roundtrip_when_configured() {
        let url = match std::env::var("RANVIER_PERSISTENCE_POSTGRES_URL") {
            Ok(value) => value,
            Err(_) => return,
        };

        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .connect(&url)
            .await
            .unwrap();
        let table_prefix = format!(
            "ranvier_compensation_idempotency_test_{}",
            Uuid::new_v4().simple()
        );
        let store =
            PostgresCompensationIdempotencyStore::with_table_prefix(pool.clone(), &table_prefix);
        store.ensure_schema().await.unwrap();

        let key = format!("trace-{}:OrderFlow:Fault", Uuid::new_v4().simple());
        assert!(!store.was_compensated(&key).await.unwrap());
        store.mark_compensated(&key).await.unwrap();
        assert!(store.was_compensated(&key).await.unwrap());
        store.mark_compensated(&key).await.unwrap();
        assert!(store.was_compensated(&key).await.unwrap());

        let drop_table = format!("DROP TABLE IF EXISTS {}", store.table);
        sqlx::query(&drop_table).execute(&pool).await.unwrap();
    }

    #[cfg(feature = "persistence-postgres")]
    #[tokio::test]
    async fn postgres_compensation_idempotency_purge_when_configured() {
        let url = match std::env::var("RANVIER_PERSISTENCE_POSTGRES_URL") {
            Ok(value) => value,
            Err(_) => return,
        };

        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .connect(&url)
            .await
            .unwrap();
        let table_prefix = format!(
            "ranvier_compensation_idempotency_purge_test_{}",
            Uuid::new_v4().simple()
        );
        let store =
            PostgresCompensationIdempotencyStore::with_table_prefix(pool.clone(), &table_prefix);
        store.ensure_schema().await.unwrap();

        let stale_key = format!("stale-{}", Uuid::new_v4().simple());
        let fresh_key = format!("fresh-{}", Uuid::new_v4().simple());
        store.mark_compensated(&stale_key).await.unwrap();
        store.mark_compensated(&fresh_key).await.unwrap();

        let force_stale_query = format!(
            "UPDATE {}
             SET created_at_ms = 0
             WHERE idempotency_key = $1",
            store.table
        );
        sqlx::query(&force_stale_query)
            .bind(&stale_key)
            .execute(&pool)
            .await
            .unwrap();

        let purged = store.purge_older_than_ms(1).await.unwrap();
        assert!(purged >= 1);
        assert!(!store.was_compensated(&stale_key).await.unwrap());
        assert!(store.was_compensated(&fresh_key).await.unwrap());

        let drop_table = format!("DROP TABLE IF EXISTS {}", store.table);
        sqlx::query(&drop_table).execute(&pool).await.unwrap();
    }

    #[cfg(feature = "persistence-redis")]
    #[tokio::test]
    async fn redis_compensation_idempotency_roundtrip_when_configured() {
        let url = match std::env::var("RANVIER_PERSISTENCE_REDIS_URL") {
            Ok(value) => value,
            Err(_) => return,
        };

        let base = RedisCompensationIdempotencyStore::connect(&url)
            .await
            .unwrap();
        let prefix = format!(
            "ranvier:compensation:idempotency:test:{}",
            Uuid::new_v4().simple()
        );
        let store = RedisCompensationIdempotencyStore::with_prefix(base.manager.clone(), prefix);
        let key = format!("trace-{}:OrderFlow:Fault", Uuid::new_v4().simple());

        assert!(!store.was_compensated(&key).await.unwrap());
        store.mark_compensated(&key).await.unwrap();
        assert!(store.was_compensated(&key).await.unwrap());
        store.mark_compensated(&key).await.unwrap();
        assert!(store.was_compensated(&key).await.unwrap());

        use redis::AsyncCommands;
        let mut conn = store.manager.clone();
        let _: () = conn.del(store.key(&key)).await.unwrap();
    }

    #[cfg(feature = "persistence-redis")]
    #[tokio::test]
    async fn redis_compensation_idempotency_ttl_when_configured() {
        let url = match std::env::var("RANVIER_PERSISTENCE_REDIS_URL") {
            Ok(value) => value,
            Err(_) => return,
        };

        let base = RedisCompensationIdempotencyStore::connect(&url)
            .await
            .unwrap();
        let prefix = format!(
            "ranvier:compensation:idempotency:ttl:test:{}",
            Uuid::new_v4().simple()
        );
        let store =
            RedisCompensationIdempotencyStore::with_prefix_and_ttl(base.manager.clone(), prefix, 1);
        let key = format!("ttl-{}", Uuid::new_v4().simple());

        assert!(!store.was_compensated(&key).await.unwrap());
        store.mark_compensated(&key).await.unwrap();
        assert!(store.was_compensated(&key).await.unwrap());

        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
        assert!(!store.was_compensated(&key).await.unwrap());
    }
}
