pub mod file_sink;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

/// Core error type for audit operations
#[derive(Debug, Error)]
pub enum AuditError {
    #[error("Failed to append event: {0}")]
    AppendFailed(String),
    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),
    #[error("Internal audit error: {0}")]
    Internal(String),
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
}

/// A sink responsible for durably and securely writing audit events.
#[async_trait]
pub trait AuditSink: Send + Sync {
    /// Append a new event to the secure audit log.
    async fn append(&self, event: &AuditEvent) -> Result<(), AuditError>;
}

/// Core interface for invoking audit logging from within Ranvier.
#[derive(Clone)]
pub struct AuditLogger<S: AuditSink> {
    sink: std::sync::Arc<S>,
}

impl<S: AuditSink> AuditLogger<S> {
    pub fn new(sink: S) -> Self {
        Self {
            sink: std::sync::Arc::new(sink),
        }
    }

    /// Logs an event
    pub async fn log(&self, event: AuditEvent) -> Result<(), AuditError> {
        self.sink.append(&event).await
    }
}

#[cfg(test)]
mod tests;
