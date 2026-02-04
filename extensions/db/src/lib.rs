// # Ranvier Database Integration
//
// This crate provides database integration patterns for Ranvier Axon applications.
// It extends the core `Transition` trait with database-specific patterns.

pub mod node;
pub mod pool;
pub mod transaction;

// Re-exports for convenience
pub use node::{DbError, DbTransition, PgNode, QueryResult, TxPgNode};
pub use pool::{DbPool, DbPoolError, MySqlPool, PoolSize, PostgresPool, SqlitePool};
pub use transaction::{
    IsolationLevel, MySqlTransaction, PgTransaction, SqliteTransaction, Transaction,
    TransactionError, TxBus,
};

// Prelude module
pub mod prelude {
    pub use crate::node::{DbError, DbTransition, PgNode, QueryResult, TxPgNode};
    pub use crate::pool::{DbPool, MySqlPool, PoolSize, PostgresPool, SqlitePool};
    pub use crate::transaction::{Transaction, TransactionError, TxBus};
}
