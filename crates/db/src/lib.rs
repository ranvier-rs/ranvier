// # Ranvier Database Integration
//
// This crate provides database integration patterns for Ranvier Axon applications.
// It extends the core `Transition` trait with database-specific patterns.

pub mod pool;
pub mod transaction;
pub mod node;

// Re-exports for convenience
pub use pool::{DbPool, PostgresPool, MySqlPool, SqlitePool, PoolSize, DbPoolError};
pub use transaction::{TxBus, Transaction, TransactionError, IsolationLevel, PgTransaction, MySqlTransaction, SqliteTransaction};
pub use node::{DbTransition, QueryResult, DbError, PgNode, TxPgNode};

// Prelude module
pub mod prelude {
    pub use crate::pool::{DbPool, PostgresPool, MySqlPool, SqlitePool, PoolSize};
    pub use crate::transaction::{TxBus, Transaction, TransactionError};
    pub use crate::node::{DbTransition, QueryResult, DbError, PgNode, TxPgNode};
}
