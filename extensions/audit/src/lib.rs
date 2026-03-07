pub mod file_sink;
#[cfg(feature = "postgres")]
pub mod postgres;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use ring::digest;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::Mutex;

/// Core error type for audit operations
#[derive(Debug, Error)]
pub enum AuditError {
    #[error("Failed to append event: {0}")]
    AppendFailed(String),
    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),
    #[error("Internal audit error: {0}")]
    Internal(String),
    #[error("Integrity violation at event index {index}: {reason}")]
    IntegrityViolation { index: usize, reason: String },
}

/// The main Audit Event payload reflecting the 5 W's.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    /// Unique event identifier for correlation
    pub id: String,
    /// When did this occur?
    pub timestamp: DateTime<Utc>,
    /// Who performed the action? (User, Service, System)
    pub actor: String,
    /// What was the action? (Create, Read, Update, Delete, Transition, etc.)
    pub action: String,
    /// Where did it happen? (Target resource identifier, node id)
    pub target: String,
    /// What was the intent or outcome?
    pub intent: Option<String>,
    /// Optional structured metadata related to the event payload.
    /// Warning: Do not embed PII here unless properly redacted.
    pub metadata: HashMap<String, serde_json::Value>,
    /// Hash of the previous event in the chain (None for the first event).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev_hash: Option<String>,
}

impl AuditEvent {
    /// Creates a new basic audit event
    pub fn new(id: String, actor: String, action: String, target: String) -> Self {
        Self {
            id,
            timestamp: Utc::now(),
            actor,
            action,
            target,
            intent: None,
            metadata: HashMap::new(),
            prev_hash: None,
        }
    }

    pub fn with_intent(mut self, intent: impl Into<String>) -> Self {
        self.intent = Some(intent.into());
        self
    }

    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Serialize) -> Self {
        if let Ok(json_val) = serde_json::to_value(value) {
            self.metadata.insert(key.into(), json_val);
        }
        self
    }

    /// Compute the SHA-256 hash of this event's canonical JSON representation.
    pub fn compute_hash(&self) -> String {
        let payload = serde_json::to_string(self).unwrap_or_default();
        let digest = digest::digest(&digest::SHA256, payload.as_bytes());
        hex::encode(digest.as_ref())
    }
}

// ---------------------------------------------------------------------------
// AuditChain — tamper-proof hash chain
// ---------------------------------------------------------------------------

/// A tamper-proof hash chain of audit events.
///
/// Each event's `prev_hash` links to the SHA-256 hash of the preceding event,
/// forming a chain. If any event is deleted or modified, the chain breaks.
#[derive(Clone)]
pub struct AuditChain {
    events: Arc<Mutex<Vec<AuditEvent>>>,
    last_hash: Arc<Mutex<Option<String>>>,
}

impl AuditChain {
    pub fn new() -> Self {
        Self {
            events: Arc::new(Mutex::new(Vec::new())),
            last_hash: Arc::new(Mutex::new(None)),
        }
    }

    /// Append an event to the chain, linking it to the previous event's hash.
    pub async fn append(&self, mut event: AuditEvent) -> AuditEvent {
        let mut events = self.events.lock().await;
        let mut last_hash = self.last_hash.lock().await;

        event.prev_hash = last_hash.clone();
        let hash = event.compute_hash();
        *last_hash = Some(hash);

        events.push(event.clone());
        event
    }

    /// Verify the integrity of the entire chain.
    ///
    /// Returns `Ok(())` if the chain is intact, or an error describing the
    /// first integrity violation found.
    pub async fn verify(&self) -> Result<(), AuditError> {
        let events = self.events.lock().await;
        let mut prev_hash: Option<String> = None;

        for (i, event) in events.iter().enumerate() {
            if event.prev_hash != prev_hash {
                return Err(AuditError::IntegrityViolation {
                    index: i,
                    reason: format!(
                        "expected prev_hash {:?}, found {:?}",
                        prev_hash, event.prev_hash
                    ),
                });
            }
            prev_hash = Some(event.compute_hash());
        }

        Ok(())
    }

    /// Get a snapshot of all events in the chain.
    pub async fn events(&self) -> Vec<AuditEvent> {
        self.events.lock().await.clone()
    }

    /// Get the number of events in the chain.
    pub async fn len(&self) -> usize {
        self.events.lock().await.len()
    }

    /// Check if the chain is empty.
    pub async fn is_empty(&self) -> bool {
        self.events.lock().await.is_empty()
    }
}

impl Default for AuditChain {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// AuditQuery — structured event filtering
// ---------------------------------------------------------------------------

/// Builder for filtering audit events by time range, event type, actor, or resource.
#[derive(Debug, Clone, Default)]
pub struct AuditQuery {
    pub(crate) time_start: Option<DateTime<Utc>>,
    pub(crate) time_end: Option<DateTime<Utc>>,
    pub(crate) action: Option<String>,
    pub(crate) actor: Option<String>,
    pub(crate) target: Option<String>,
}

impl AuditQuery {
    pub fn new() -> Self {
        Self::default()
    }

    /// Filter events within a time range (inclusive).
    pub fn time_range(mut self, start: DateTime<Utc>, end: DateTime<Utc>) -> Self {
        self.time_start = Some(start);
        self.time_end = Some(end);
        self
    }

    /// Filter events by action type.
    pub fn action(mut self, action: impl Into<String>) -> Self {
        self.action = Some(action.into());
        self
    }

    /// Filter events by actor.
    pub fn actor(mut self, actor: impl Into<String>) -> Self {
        self.actor = Some(actor.into());
        self
    }

    /// Filter events by target resource.
    pub fn target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }

    /// Test whether an event matches this query.
    pub fn matches(&self, event: &AuditEvent) -> bool {
        if let Some(start) = &self.time_start {
            if event.timestamp < *start {
                return false;
            }
        }
        if let Some(end) = &self.time_end {
            if event.timestamp > *end {
                return false;
            }
        }
        if let Some(action) = &self.action {
            if event.action != *action {
                return false;
            }
        }
        if let Some(actor) = &self.actor {
            if event.actor != *actor {
                return false;
            }
        }
        if let Some(target) = &self.target {
            if event.target != *target {
                return false;
            }
        }
        true
    }

    /// Apply this query to a slice of events, returning matching events.
    pub fn filter<'a>(&self, events: &'a [AuditEvent]) -> Vec<&'a AuditEvent> {
        events.iter().filter(|e| self.matches(e)).collect()
    }
}

// ---------------------------------------------------------------------------
// RetentionPolicy — automatic event lifecycle management
// ---------------------------------------------------------------------------

/// Strategy for handling events that exceed retention limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArchiveStrategy {
    /// Delete events permanently.
    Delete,
    /// Archive events (caller handles archival destination).
    Archive,
}

/// Policy for automatic audit event lifecycle management.
#[derive(Debug, Clone)]
pub struct RetentionPolicy {
    /// Maximum age of events to retain.
    pub max_age: Option<Duration>,
    /// Maximum number of events to retain.
    pub max_count: Option<usize>,
    /// What to do with events that exceed retention limits.
    pub strategy: ArchiveStrategy,
}

impl RetentionPolicy {
    /// Create a retention policy with a maximum event age.
    pub fn max_age(age: Duration) -> Self {
        Self {
            max_age: Some(age),
            max_count: None,
            strategy: ArchiveStrategy::Delete,
        }
    }

    /// Create a retention policy with a maximum event count.
    pub fn max_count(count: usize) -> Self {
        Self {
            max_age: None,
            max_count: Some(count),
            strategy: ArchiveStrategy::Delete,
        }
    }

    /// Set the archive strategy.
    pub fn with_strategy(mut self, strategy: ArchiveStrategy) -> Self {
        self.strategy = strategy;
        self
    }

    /// Apply this retention policy to a list of events, returning
    /// `(retained, expired)` event lists.
    pub fn apply(&self, events: &[AuditEvent]) -> (Vec<AuditEvent>, Vec<AuditEvent>) {
        let now = Utc::now();
        let mut retained: Vec<AuditEvent> = events.to_vec();
        let mut expired: Vec<AuditEvent> = Vec::new();

        // Apply max_age filter
        if let Some(max_age) = &self.max_age {
            let cutoff = now - *max_age;
            let (keep, remove): (Vec<_>, Vec<_>) =
                retained.into_iter().partition(|e| e.timestamp >= cutoff);
            retained = keep;
            expired.extend(remove);
        }

        // Apply max_count (keep the newest events)
        if let Some(max_count) = self.max_count {
            if retained.len() > max_count {
                let excess = retained.len() - max_count;
                let removed: Vec<_> = retained.drain(..excess).collect();
                expired.extend(removed);
            }
        }

        (retained, expired)
    }
}

// ---------------------------------------------------------------------------
// AuditSink trait
// ---------------------------------------------------------------------------

/// A sink responsible for durably and securely writing audit events.
#[async_trait]
pub trait AuditSink: Send + Sync {
    /// Append a new event to the secure audit log.
    async fn append(&self, event: &AuditEvent) -> Result<(), AuditError>;

    /// Query events matching the given filter. Not all sinks support querying.
    async fn query(&self, _query: &AuditQuery) -> Result<Vec<AuditEvent>, AuditError> {
        Ok(Vec::new())
    }

    /// Apply a retention policy. Not all sinks support retention.
    async fn apply_retention(
        &self,
        _policy: &RetentionPolicy,
    ) -> Result<Vec<AuditEvent>, AuditError> {
        Ok(Vec::new())
    }
}

// ---------------------------------------------------------------------------
// InMemoryAuditSink — in-memory sink with query and retention support
// ---------------------------------------------------------------------------

/// In-memory audit sink for testing and development.
///
/// Supports querying and retention policy application.
#[derive(Clone, Default)]
pub struct InMemoryAuditSink {
    events: Arc<Mutex<Vec<AuditEvent>>>,
}

impl InMemoryAuditSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get a snapshot of all stored events.
    pub async fn get_events(&self) -> Vec<AuditEvent> {
        self.events.lock().await.clone()
    }

    /// Get the number of stored events.
    pub async fn len(&self) -> usize {
        self.events.lock().await.len()
    }

    /// Check if the sink is empty.
    pub async fn is_empty(&self) -> bool {
        self.events.lock().await.is_empty()
    }
}

#[async_trait]
impl AuditSink for InMemoryAuditSink {
    async fn append(&self, event: &AuditEvent) -> Result<(), AuditError> {
        self.events.lock().await.push(event.clone());
        Ok(())
    }

    async fn query(&self, query: &AuditQuery) -> Result<Vec<AuditEvent>, AuditError> {
        let events = self.events.lock().await;
        Ok(query.filter(&events).into_iter().cloned().collect())
    }

    async fn apply_retention(
        &self,
        policy: &RetentionPolicy,
    ) -> Result<Vec<AuditEvent>, AuditError> {
        let mut events = self.events.lock().await;
        let (retained, expired) = policy.apply(&events);
        *events = retained;
        Ok(expired)
    }
}

// ---------------------------------------------------------------------------
// AuditLogger
// ---------------------------------------------------------------------------

/// Core interface for invoking audit logging from within Ranvier.
#[derive(Clone)]
pub struct AuditLogger<S: AuditSink> {
    sink: Arc<S>,
}

impl<S: AuditSink> AuditLogger<S> {
    pub fn new(sink: S) -> Self {
        Self {
            sink: Arc::new(sink),
        }
    }

    /// Logs an event
    pub async fn log(&self, event: AuditEvent) -> Result<(), AuditError> {
        self.sink.append(&event).await
    }

    /// Query events matching the given filter.
    pub async fn query(&self, query: &AuditQuery) -> Result<Vec<AuditEvent>, AuditError> {
        self.sink.query(query).await
    }

    /// Apply a retention policy.
    pub async fn apply_retention(
        &self,
        policy: &RetentionPolicy,
    ) -> Result<Vec<AuditEvent>, AuditError> {
        self.sink.apply_retention(policy).await
    }
}

#[cfg(test)]
mod tests;
