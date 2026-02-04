//! Transaction Support for Ranvier Database Operations
//!
//! Provides `TxBus` - a Bus wrapper that manages database transactions
//! with automatic commit/rollback based on `Outcome`.

use async_trait::async_trait;
use ranvier_core::bus::Bus;
use ranvier_core::outcome::Outcome;
use std::ops::{Deref, DerefMut};

/// Transaction isolation levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationLevel {
    /// Read uncommitted (lowest isolation)
    ReadUncommitted,
    /// Read committed
    ReadCommitted,
    /// Repeatable read
    RepeatableRead,
    /// Serializable (highest isolation)
    Serializable,
}

/// Transaction-related errors.
#[derive(Debug, thiserror::Error)]
pub enum TransactionError {
    #[error("Failed to begin transaction: {0}")]
    BeginFailed(String),

    #[error("Failed to commit transaction: {0}")]
    CommitFailed(String),

    #[error("Failed to rollback transaction: {0}")]
    RollbackFailed(String),

    #[error("Transaction already completed")]
    AlreadyCompleted,

    #[error("No active transaction in context")]
    NoTransaction,

    #[error("Backend error: {0}")]
    Backend(String),
}

/// Result type for database operations within transactions.
pub type TxResult<T> = Result<T, TransactionError>;

// ============== Transaction Trait ==============

/// Core transaction abstraction.
///
/// `Transaction` provides a generic interface over different database
/// transaction types (PostgreSQL, MySQL, SQLite).
#[async_trait]
pub trait Transaction: Send + Sync + 'static {
    /// Commit the transaction.
    async fn commit(self: Box<Self>) -> Result<(), TransactionError>;

    /// Rollback the transaction.
    async fn rollback(self: Box<Self>) -> Result<(), TransactionError>;
}

// ============== PostgreSQL Transaction ==============

/// PostgreSQL transaction wrapper.
pub struct PgTransaction {
    inner: sqlx::Transaction<'static, sqlx::Postgres>,
    completed: bool,
}

impl PgTransaction {
    pub(crate) fn new(tx: sqlx::Transaction<'static, sqlx::Postgres>) -> Self {
        Self {
            inner: tx,
            completed: false,
        }
    }

    /// Get the underlying SQLx transaction.
    pub fn inner(&mut self) -> &mut sqlx::Transaction<'static, sqlx::Postgres> {
        &mut self.inner
    }
}

#[async_trait]
impl Transaction for PgTransaction {
    async fn commit(self: Box<Self>) -> Result<(), TransactionError> {
        if self.completed {
            return Err(TransactionError::AlreadyCompleted);
        }
        self.inner
            .commit()
            .await
            .map_err(|e| TransactionError::CommitFailed(e.to_string()))
    }

    async fn rollback(self: Box<Self>) -> Result<(), TransactionError> {
        if self.completed {
            return Err(TransactionError::AlreadyCompleted);
        }
        self.inner
            .rollback()
            .await
            .map_err(|e| TransactionError::RollbackFailed(e.to_string()))
    }
}

// ============== MySQL Transaction ==============

/// MySQL transaction wrapper.
pub struct MySqlTransaction {
    inner: sqlx::Transaction<'static, sqlx::MySql>,
    completed: bool,
}

impl MySqlTransaction {
    pub(crate) fn new(tx: sqlx::Transaction<'static, sqlx::MySql>) -> Self {
        Self {
            inner: tx,
            completed: false,
        }
    }

    /// Get the underlying SQLx transaction.
    pub fn inner(&mut self) -> &mut sqlx::Transaction<'static, sqlx::MySql> {
        &mut self.inner
    }
}

#[async_trait]
impl Transaction for MySqlTransaction {
    async fn commit(self: Box<Self>) -> Result<(), TransactionError> {
        if self.completed {
            return Err(TransactionError::AlreadyCompleted);
        }
        self.inner
            .commit()
            .await
            .map_err(|e| TransactionError::CommitFailed(e.to_string()))
    }

    async fn rollback(self: Box<Self>) -> Result<(), TransactionError> {
        if self.completed {
            return Err(TransactionError::AlreadyCompleted);
        }
        self.inner
            .rollback()
            .await
            .map_err(|e| TransactionError::RollbackFailed(e.to_string()))
    }
}

// ============== SQLite Transaction ==============

/// SQLite transaction wrapper.
pub struct SqliteTransaction {
    inner: sqlx::Transaction<'static, sqlx::Sqlite>,
    completed: bool,
}

impl SqliteTransaction {
    pub(crate) fn new(tx: sqlx::Transaction<'static, sqlx::Sqlite>) -> Self {
        Self {
            inner: tx,
            completed: false,
        }
    }

    /// Get the underlying SQLx transaction.
    pub fn inner(&mut self) -> &mut sqlx::Transaction<'static, sqlx::Sqlite> {
        &mut self.inner
    }
}

#[async_trait]
impl Transaction for SqliteTransaction {
    async fn commit(self: Box<Self>) -> Result<(), TransactionError> {
        if self.completed {
            return Err(TransactionError::AlreadyCompleted);
        }
        self.inner
            .commit()
            .await
            .map_err(|e| TransactionError::CommitFailed(e.to_string()))
    }

    async fn rollback(self: Box<Self>) -> Result<(), TransactionError> {
        if self.completed {
            return Err(TransactionError::AlreadyCompleted);
        }
        self.inner
            .rollback()
            .await
            .map_err(|e| TransactionError::RollbackFailed(e.to_string()))
    }
}

// ============== TxBus ==============

/// A Bus wrapper that manages an active database transaction.
///
/// `TxBus` ensures that transactions are properly committed or rolled back
/// based on the `Outcome` of the Axon execution:
/// - `Outcome::Next` → Commit
/// - `Outcome::Fault` → Rollback
/// - `Outcome::Branch` → Commit (before branching)
/// - `Outcome::Jump` → Commit (before jumping)
///
/// Example:
/// ```rust,ignore
/// let tx_bus = TxBus::new(bus, pool.begin().await?);
/// let outcome = axon.run(input, &mut tx_bus).await?;
/// tx_bus.finalize(outcome).await?; // Auto commit/rollback
/// ```
pub struct TxBus {
    inner: Bus,
    tx: Option<Box<dyn Transaction + Send>>,
}

impl TxBus {
    /// Create a new `TxBus` with an active transaction.
    pub fn new(bus: Bus, tx: Box<dyn Transaction + Send>) -> Self {
        Self {
            inner: bus,
            tx: Some(tx),
        }
    }

    /// Create a `TxBus` from a PostgreSQL transaction.
    pub fn from_pg(bus: Bus, tx: sqlx::Transaction<'static, sqlx::Postgres>) -> Self {
        Self::new(bus, Box::new(PgTransaction::new(tx)))
    }

    /// Create a `TxBus` from a MySQL transaction.
    pub fn from_mysql(bus: Bus, tx: sqlx::Transaction<'static, sqlx::MySql>) -> Self {
        Self::new(bus, Box::new(MySqlTransaction::new(tx)))
    }

    /// Create a `TxBus` from a SQLite transaction.
    pub fn from_sqlite(bus: Bus, tx: sqlx::Transaction<'static, sqlx::Sqlite>) -> Self {
        Self::new(bus, Box::new(SqliteTransaction::new(tx)))
    }

    /// Finalize the transaction based on the Outcome.
    ///
    /// - Success outcomes (Next, Branch, Jump) → Commit
    /// - Error outcomes (Fault) → Rollback
    pub async fn finalize<T, E>(
        mut self,
        outcome: Outcome<T, E>,
    ) -> Result<Outcome<T, E>, TransactionError> {
        if let Some(tx) = self.tx.take() {
            match &outcome {
                Outcome::Next(_)
                | Outcome::Branch(_, _)
                | Outcome::Jump(_, _)
                | Outcome::Emit(_, _) => {
                    tx.commit().await?;
                }
                Outcome::Fault(_) => {
                    tx.rollback().await?;
                }
            }
        }
        Ok(outcome)
    }

    /// Manually commit the transaction.
    pub async fn commit(&mut self) -> Result<(), TransactionError> {
        if let Some(tx) = self.tx.take() {
            return tx.commit().await;
        }
        Err(TransactionError::AlreadyCompleted)
    }

    /// Manually rollback the transaction.
    pub async fn rollback(&mut self) -> Result<(), TransactionError> {
        if let Some(tx) = self.tx.take() {
            return tx.rollback().await;
        }
        Err(TransactionError::AlreadyCompleted)
    }

    /// Check if a transaction is still active.
    pub fn is_active(&self) -> bool {
        self.tx.is_some()
    }
}

impl Deref for TxBus {
    type Target = Bus;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for TxBus {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}
