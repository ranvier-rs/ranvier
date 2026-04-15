//! PostgreSQL-backed audit sink using sqlx.
//!
//! Requires the `postgres` feature flag.
//!
//! # Table Schema
//!
//! ```sql
//! CREATE TABLE IF NOT EXISTS audit_events (
//!     id          TEXT PRIMARY KEY,
//!     timestamp   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
//!     actor       TEXT NOT NULL,
//!     action      TEXT NOT NULL,
//!     target      TEXT NOT NULL,
//!     intent      TEXT,
//!     metadata    JSONB NOT NULL DEFAULT '{}',
//!     prev_hash   TEXT
//! );
//! ```
//!
//! The migration SQL is bundled at `migrations/001_create_audit_events.sql`.

use crate::{AuditError, AuditEvent, AuditQuery, AuditSink, RetentionPolicy};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use std::collections::HashMap;

/// Migration SQL for creating the audit_events table.
pub const MIGRATION_SQL: &str = include_str!("../migrations/001_create_audit_events.sql");

/// Configuration for [`PostgresAuditSink`] connection pool.
#[derive(Debug, Clone)]
pub struct PostgresAuditConfig {
    /// PostgreSQL connection URL (e.g., `postgres://user:pass@localhost/db`).
    pub url: String,
    /// Maximum number of connections in the pool (default: 5).
    pub max_connections: u32,
    /// Minimum number of connections to keep open (default: 1).
    pub min_connections: u32,
    /// Table name (default: `"audit_events"`).
    pub table_name: String,
}

impl PostgresAuditConfig {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            max_connections: 5,
            min_connections: 1,
            table_name: "audit_events".to_string(),
        }
    }

    pub fn max_connections(mut self, n: u32) -> Self {
        self.max_connections = n;
        self
    }

    pub fn min_connections(mut self, n: u32) -> Self {
        self.min_connections = n;
        self
    }

    /// Set the table name for audit events.
    ///
    /// The name is validated against `[a-zA-Z_][a-zA-Z0-9_]{0,62}` to prevent
    /// SQL injection (table names cannot be parameterized in PostgreSQL).
    pub fn table_name(mut self, name: impl Into<String>) -> Result<Self, AuditError> {
        let name = name.into();
        validate_table_name(&name)?;
        self.table_name = name;
        Ok(self)
    }
}

/// Validate a SQL table name to prevent injection attacks.
///
/// Only allows identifiers matching `[a-zA-Z_][a-zA-Z0-9_]{0,62}`.
/// PostgreSQL limits unquoted identifiers to 63 bytes (NAMEDATALEN - 1).
fn validate_table_name(name: &str) -> Result<(), AuditError> {
    if name.is_empty() || name.len() > 63 {
        return Err(AuditError::InvalidTableName(format!(
            "table name must be 1-63 characters, got {}",
            name.len()
        )));
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphabetic() && first != '_' {
        return Err(AuditError::InvalidTableName(format!(
            "must start with ASCII letter or underscore, got '{first}'"
        )));
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(AuditError::InvalidTableName(format!(
            "contains invalid characters: '{name}'"
        )));
    }
    Ok(())
}

/// PostgreSQL-backed audit sink with hash chain integrity.
///
/// Each appended event is linked to the previous event via `prev_hash`,
/// forming a tamper-proof chain stored in PostgreSQL.
pub struct PostgresAuditSink {
    pool: PgPool,
    table_name: String,
}

impl PostgresAuditSink {
    /// Connect to PostgreSQL and create the sink.
    pub async fn connect(config: &PostgresAuditConfig) -> Result<Self, AuditError> {
        let pool = PgPoolOptions::new()
            .max_connections(config.max_connections)
            .min_connections(config.min_connections)
            .connect(&config.url)
            .await
            .map_err(|e| AuditError::Internal(format!("pool connection failed: {e}")))?;

        Ok(Self {
            pool,
            table_name: config.table_name.clone(),
        })
    }

    /// Create a sink from an existing pool (useful for testing).
    pub fn from_pool(pool: PgPool, table_name: impl Into<String>) -> Self {
        Self {
            pool,
            table_name: table_name.into(),
        }
    }

    /// Run the migration to create the audit_events table.
    pub async fn migrate(&self) -> Result<(), AuditError> {
        sqlx::query(MIGRATION_SQL)
            .execute(&self.pool)
            .await
            .map_err(|e| AuditError::Internal(format!("migration failed: {e}")))?;
        Ok(())
    }

    /// Fetch the hash of the most recent event in the table.
    pub async fn last_hash(&self) -> Result<Option<String>, AuditError> {
        let last_event = self.fetch_last_event().await?;
        Ok(last_event.map(|e| e.compute_hash()))
    }

    /// Fetch the last inserted event.
    pub async fn fetch_last_event(&self) -> Result<Option<AuditEvent>, AuditError> {
        let sql = format!(
            "SELECT id, timestamp, actor, action, target, intent, metadata, prev_hash \
             FROM {} ORDER BY timestamp DESC, id DESC LIMIT 1",
            self.table_name
        );
        let row = sqlx::query(&sql)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| AuditError::Internal(e.to_string()))?;

        match row {
            Some(r) => Ok(Some(row_to_event(&r)?)),
            None => Ok(None),
        }
    }

    /// Verify the integrity of the hash chain stored in the table.
    ///
    /// Reads all events in order and checks that each event's `prev_hash`
    /// matches the computed hash of the preceding event.
    pub async fn verify_chain(&self) -> Result<(), AuditError> {
        let sql = format!(
            "SELECT id, timestamp, actor, action, target, intent, metadata, prev_hash \
             FROM {} ORDER BY timestamp ASC, id ASC",
            self.table_name
        );
        let rows = sqlx::query(&sql)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| AuditError::Internal(e.to_string()))?;

        let mut prev_hash: Option<String> = None;
        for (i, row) in rows.iter().enumerate() {
            let event = row_to_event(row)?;
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

    /// Build a WHERE clause from an AuditQuery.
    pub(crate) fn build_where_clause(query: &AuditQuery) -> (String, Vec<QueryParam>) {
        let mut conditions = Vec::new();
        let mut params = Vec::new();

        if let Some(ref action) = query.action {
            params.push(QueryParam::Text(action.clone()));
            conditions.push(format!("action = ${}", params.len()));
        }
        if let Some(ref actor) = query.actor {
            params.push(QueryParam::Text(actor.clone()));
            conditions.push(format!("actor = ${}", params.len()));
        }
        if let Some(ref target) = query.target {
            params.push(QueryParam::Text(target.clone()));
            conditions.push(format!("target = ${}", params.len()));
        }
        if let Some(ref start) = query.time_start {
            params.push(QueryParam::Timestamp(*start));
            conditions.push(format!("timestamp >= ${}", params.len()));
        }
        if let Some(ref end) = query.time_end {
            params.push(QueryParam::Timestamp(*end));
            conditions.push(format!("timestamp <= ${}", params.len()));
        }

        if conditions.is_empty() {
            (String::new(), params)
        } else {
            (format!("WHERE {}", conditions.join(" AND ")), params)
        }
    }
}

/// Internal parameter type for query building.
#[derive(Debug, Clone)]
pub(crate) enum QueryParam {
    Text(String),
    Timestamp(DateTime<Utc>),
}

/// Convert a sqlx Row to an AuditEvent.
fn row_to_event(row: &sqlx::postgres::PgRow) -> Result<AuditEvent, AuditError> {
    let metadata_json: serde_json::Value = row
        .try_get("metadata")
        .map_err(|e| AuditError::Internal(e.to_string()))?;
    let metadata: HashMap<String, serde_json::Value> =
        serde_json::from_value(metadata_json).unwrap_or_default();

    Ok(AuditEvent {
        id: row
            .try_get("id")
            .map_err(|e| AuditError::Internal(e.to_string()))?,
        timestamp: row
            .try_get("timestamp")
            .map_err(|e| AuditError::Internal(e.to_string()))?,
        actor: row
            .try_get("actor")
            .map_err(|e| AuditError::Internal(e.to_string()))?,
        action: row
            .try_get("action")
            .map_err(|e| AuditError::Internal(e.to_string()))?,
        target: row
            .try_get("target")
            .map_err(|e| AuditError::Internal(e.to_string()))?,
        intent: row
            .try_get("intent")
            .map_err(|e| AuditError::Internal(e.to_string()))?,
        metadata,
        prev_hash: row
            .try_get("prev_hash")
            .map_err(|e| AuditError::Internal(e.to_string()))?,
    })
}

#[async_trait]
impl AuditSink for PostgresAuditSink {
    async fn append(&self, event: &AuditEvent) -> Result<(), AuditError> {
        let metadata_json = serde_json::to_value(&event.metadata)?;
        let sql = format!(
            "INSERT INTO {} (id, timestamp, actor, action, target, intent, metadata, prev_hash) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            self.table_name
        );

        sqlx::query(&sql)
            .bind(&event.id)
            .bind(event.timestamp)
            .bind(&event.actor)
            .bind(&event.action)
            .bind(&event.target)
            .bind(&event.intent)
            .bind(metadata_json)
            .bind(&event.prev_hash)
            .execute(&self.pool)
            .await
            .map_err(|e| AuditError::AppendFailed(e.to_string()))?;

        Ok(())
    }

    async fn query(&self, query: &AuditQuery) -> Result<Vec<AuditEvent>, AuditError> {
        let (where_clause, params) = Self::build_where_clause(query);
        let sql = format!(
            "SELECT id, timestamp, actor, action, target, intent, metadata, prev_hash \
             FROM {} {} ORDER BY timestamp ASC",
            self.table_name, where_clause
        );

        let mut q = sqlx::query(&sql);
        for param in &params {
            q = match param {
                QueryParam::Text(t) => q.bind(t),
                QueryParam::Timestamp(ts) => q.bind(ts),
            };
        }

        let rows = q
            .fetch_all(&self.pool)
            .await
            .map_err(|e| AuditError::Internal(e.to_string()))?;

        let mut events = Vec::with_capacity(rows.len());
        for row in &rows {
            events.push(row_to_event(row)?);
        }
        Ok(events)
    }

    async fn apply_retention(
        &self,
        policy: &RetentionPolicy,
    ) -> Result<Vec<AuditEvent>, AuditError> {
        let mut conditions = Vec::new();

        if let Some(ref max_age) = policy.max_age {
            let cutoff = Utc::now() - *max_age;
            conditions.push(format!("timestamp < '{}'", cutoff.to_rfc3339()));
        }

        if conditions.is_empty() && policy.max_count.is_none() {
            return Ok(Vec::new());
        }

        // For max_count, delete the oldest events beyond the limit
        if let Some(max_count) = policy.max_count {
            let count_sql = format!("SELECT COUNT(*) as cnt FROM {}", self.table_name);
            let row = sqlx::query(&count_sql)
                .fetch_one(&self.pool)
                .await
                .map_err(|e| AuditError::Internal(e.to_string()))?;
            let count: i64 = row.try_get("cnt").unwrap_or(0);

            if count as usize > max_count {
                let excess = count as usize - max_count;
                let delete_sql = format!(
                    "DELETE FROM {} WHERE id IN \
                     (SELECT id FROM {} ORDER BY timestamp ASC LIMIT {})",
                    self.table_name, self.table_name, excess
                );
                sqlx::query(&delete_sql)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| AuditError::Internal(e.to_string()))?;
            }
        }

        // For max_age, delete old events
        if let Some(ref max_age) = policy.max_age {
            let cutoff = Utc::now() - *max_age;
            let delete_sql = format!("DELETE FROM {} WHERE timestamp < $1", self.table_name);
            sqlx::query(&delete_sql)
                .bind(cutoff)
                .execute(&self.pool)
                .await
                .map_err(|e| AuditError::Internal(e.to_string()))?;
        }

        Ok(Vec::new())
    }
}

// Make AuditQuery fields accessible for SQL building
impl AuditQuery {
    /// Access the action filter (for SQL building).
    pub fn action_filter(&self) -> Option<&str> {
        self.action.as_deref()
    }

    /// Access the actor filter (for SQL building).
    pub fn actor_filter(&self) -> Option<&str> {
        self.actor.as_deref()
    }

    /// Access the target filter (for SQL building).
    pub fn target_filter(&self) -> Option<&str> {
        self.target.as_deref()
    }

    /// Access the time range start (for SQL building).
    pub fn time_start(&self) -> Option<DateTime<Utc>> {
        self.time_start
    }

    /// Access the time range end (for SQL building).
    pub fn time_end(&self) -> Option<DateTime<Utc>> {
        self.time_end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_sql_contains_create_table() {
        assert!(MIGRATION_SQL.contains("CREATE TABLE"));
        assert!(MIGRATION_SQL.contains("audit_events"));
        assert!(MIGRATION_SQL.contains("prev_hash"));
    }

    #[test]
    fn migration_sql_has_indexes() {
        assert!(MIGRATION_SQL.contains("idx_audit_events_actor"));
        assert!(MIGRATION_SQL.contains("idx_audit_events_action"));
        assert!(MIGRATION_SQL.contains("idx_audit_events_target"));
        assert!(MIGRATION_SQL.contains("idx_audit_events_timestamp"));
    }

    #[test]
    fn config_defaults() {
        let cfg = PostgresAuditConfig::new("postgres://localhost/test");
        assert_eq!(cfg.max_connections, 5);
        assert_eq!(cfg.min_connections, 1);
        assert_eq!(cfg.table_name, "audit_events");
    }

    #[test]
    fn config_builder() {
        let cfg = PostgresAuditConfig::new("postgres://localhost/test")
            .max_connections(10)
            .min_connections(2)
            .table_name("custom_audit")
            .unwrap();
        assert_eq!(cfg.max_connections, 10);
        assert_eq!(cfg.min_connections, 2);
        assert_eq!(cfg.table_name, "custom_audit");
    }

    #[test]
    fn table_name_rejects_sql_injection() {
        let result = PostgresAuditConfig::new("postgres://localhost/test")
            .table_name("events; DROP TABLE users; --");
        assert!(result.is_err());
    }

    #[test]
    fn table_name_rejects_empty() {
        let result = PostgresAuditConfig::new("postgres://localhost/test").table_name("");
        assert!(result.is_err());
    }

    #[test]
    fn table_name_rejects_leading_digit() {
        let result = PostgresAuditConfig::new("postgres://localhost/test").table_name("1events");
        assert!(result.is_err());
    }

    #[test]
    fn table_name_accepts_underscore_prefix() {
        let cfg = PostgresAuditConfig::new("postgres://localhost/test")
            .table_name("_audit_events")
            .unwrap();
        assert_eq!(cfg.table_name, "_audit_events");
    }

    #[test]
    fn build_where_empty_query() {
        let query = AuditQuery::new();
        let (clause, params) = PostgresAuditSink::build_where_clause(&query);
        assert!(clause.is_empty());
        assert!(params.is_empty());
    }

    #[test]
    fn build_where_single_action() {
        let query = AuditQuery::new().action("CREATE");
        let (clause, params) = PostgresAuditSink::build_where_clause(&query);
        assert_eq!(clause, "WHERE action = $1");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn build_where_multiple_filters() {
        let query = AuditQuery::new()
            .action("DELETE")
            .actor("admin")
            .target("user:42");
        let (clause, params) = PostgresAuditSink::build_where_clause(&query);
        assert!(clause.contains("action = $1"));
        assert!(clause.contains("actor = $2"));
        assert!(clause.contains("target = $3"));
        assert_eq!(params.len(), 3);
    }

    #[test]
    fn build_where_time_range() {
        let start = Utc::now() - chrono::Duration::hours(1);
        let end = Utc::now();
        let query = AuditQuery::new().time_range(start, end);
        let (clause, params) = PostgresAuditSink::build_where_clause(&query);
        assert!(clause.contains("timestamp >= $1"));
        assert!(clause.contains("timestamp <= $2"));
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn build_where_all_filters() {
        let start = Utc::now() - chrono::Duration::hours(1);
        let end = Utc::now();
        let query = AuditQuery::new()
            .action("UPDATE")
            .actor("service-a")
            .target("order:99")
            .time_range(start, end);
        let (clause, params) = PostgresAuditSink::build_where_clause(&query);
        assert!(clause.starts_with("WHERE "));
        assert_eq!(params.len(), 5);
    }

    #[test]
    fn event_hash_chain_logic() {
        // Verify that the chain linking logic works correctly
        let mut e1 = AuditEvent::new(
            "e1".into(),
            "actor".into(),
            "CREATE".into(),
            "target".into(),
        );
        e1.prev_hash = None;
        let h1 = e1.compute_hash();

        let mut e2 = AuditEvent::new(
            "e2".into(),
            "actor".into(),
            "UPDATE".into(),
            "target".into(),
        );
        e2.prev_hash = Some(h1.clone());
        let h2 = e2.compute_hash();

        // Verify chain: e2.prev_hash should equal e1's hash
        assert_eq!(e2.prev_hash.as_deref(), Some(h1.as_str()));
        // Different events produce different hashes
        assert_ne!(h1, h2);
    }
}
