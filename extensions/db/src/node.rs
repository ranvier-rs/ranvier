//! Database-Aware Node Types for Ranvier Axon
//!
//! Provides `DbNode` and `DbTransition` traits for database operations
//! that integrate with the Axon execution model.

use crate::transaction::IsolationLevel;
use async_trait::async_trait;
use ranvier_core::bus::Bus;
use ranvier_core::outcome::Outcome;
use ranvier_core::transition::Transition;

/// Database operation errors.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("Query failed: {0}")]
    QueryFailed(String),

    #[error("No rows returned")]
    NoRows,

    #[error("Connection not available")]
    NoConnection,

    #[error("Transaction error: {0}")]
    TransactionError(String),

    #[error("Parse error: {0}")]
    ParseError(String),

    #[error("Constraint violation: {0}")]
    ConstraintViolation(String),

    #[error("Unique constraint violation: {0}")]
    UniqueViolation(String),
}

/// Result type for database queries.
pub type QueryResult<T> = Result<T, DbError>;

/// Resource requirement for database operations.
pub trait DbResources: ranvier_core::transition::ResourceRequirement {
    fn pg_pool(&self) -> &sqlx::PgPool;
}

// ============== DbTransition Trait ==============

/// A transition that performs database operations with direct pool access.
///
/// `DbTransition` receives the database pool directly, allowing you to
/// execute SQL queries without manual Bus management.
///
/// Example:
/// ```rust,ignore
/// use ranvier_db::prelude::*;
///
/// struct GetUserById;
///
/// #[async_trait]
/// impl DbTransition<UserId, User> for GetUserById {
///     type Error = AppError;
///
///     async fn run(
///         &self,
///         input: UserId,
///         pool: &sqlx::PgPool,
///     ) -> QueryResult<User> {
///         sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = $1")
///             .bind(input.0)
///             .fetch_one(pool)
///             .await
///             .map_err(|e| DbError::QueryFailed(e.to_string()))
///     }
/// }
/// ```
#[async_trait]
pub trait DbTransition<From, To>: Send + Sync + 'static
where
    From: Send + Sync + 'static,
    To: Send + Sync + 'static,
{
    /// Domain-specific error type (converted to DbError on failures)
    type Error: Send + Sync + 'static;

    /// Returns information about the SQL operation for observability.
    /// Can return the SQL string (sanitized) or table names.
    fn sql_info(&self) -> Option<String> {
        None
    }

    /// Execute the database transition with pool access.
    ///
    /// This method receives the input state and the database pool,
    /// and should return the result of the database operation.
    async fn run(&self, input: From, pool: &sqlx::PgPool) -> QueryResult<To>;
}

// ============== PgNode ==============

/// PostgreSQL-backed node for Axon chains.
///
/// `PgNode` reads a `PostgresPool` from the Bus and provides it
/// to the `DbTransition` for execution.
///
/// Example:
/// ```rust,ignore
/// let axon = Axon::new()
///     .then(PgNode::new(GetUserById))
///     .then(PgNode::new(ValidateUser))
///     .branch(|user| match user.role { ... });
/// ```
pub struct PgNode<T, From, To, R>
where
    T: DbTransition<From, To> + Send + Sync,
    From: Send + Sync + 'static,
    To: Send + Sync + 'static,
    R: DbResources,
{
    inner: T,
    _phantom: std::marker::PhantomData<(From, To, R)>,
}

impl<T, From, To, R> PgNode<T, From, To, R>
where
    T: DbTransition<From, To> + Send + Sync,
    From: Send + Sync + 'static,
    To: Send + Sync + 'static,
    R: DbResources,
{
    /// Create a new PostgreSQL node.
    pub fn new(inner: T) -> Self {
        Self {
            inner,
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<T, From, To, R> Clone for PgNode<T, From, To, R>
where
    T: DbTransition<From, To> + Send + Sync + Clone,
    From: Send + Sync + 'static,
    To: Send + Sync + 'static,
    R: DbResources,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _phantom: std::marker::PhantomData,
        }
    }
}

#[async_trait]
impl<T, From, To, R> Transition<From, To> for PgNode<T, From, To, R>
where
    T: DbTransition<From, To> + Send + Sync,
    From: Send + Sync + 'static,
    To: Send + Sync + 'static,
    R: DbResources,
{
    type Error = String;
    type Resources = R;

    async fn run(
        &self,
        input: From,
        resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<To, Self::Error> {
        let pool = resources.pg_pool();

        if let Some(sql) = self.inner.sql_info() {
            tracing::debug!(ranvier.db.sql = %sql, "Executing database operation");
        }

        match self.inner.run(input, pool).await {
            Ok(result) => Outcome::Next(result),
            Err(DbError::NoRows) => Outcome::Fault("Record not found".to_string()),
            Err(e) => Outcome::Fault(format!("Database error: {}", e)),
        }
    }
}

// ============== TxPgNode ==============

/// PostgreSQL node with automatic transaction management.
///
/// `TxPgNode` wraps each execution in a transaction:
/// - On success (Next, Branch, Jump, Emit) → Commit
/// - On error (Fault) → Rollback
///
/// Example:
/// ```rust,ignore
/// let axon = Axon::new()
///     .then(TxPgNode::new(CreateUserOrder))
///     .then(TxPgNode::new(UpdateInventory));
/// ```
pub struct TxPgNode<T, From, To, R>
where
    T: DbTransition<From, To> + Send + Sync,
    From: Send + Sync + 'static,
    To: Send + Sync + 'static,
    R: DbResources,
{
    inner: T,
    isolation_level: Option<IsolationLevel>,
    _phantom: std::marker::PhantomData<(From, To, R)>,
}

impl<T, From, To, R> TxPgNode<T, From, To, R>
where
    T: DbTransition<From, To> + Send + Sync,
    From: Send + Sync + 'static,
    To: Send + Sync + 'static,
    R: DbResources,
{
    /// Create a new transactional PostgreSQL node.
    pub fn new(inner: T) -> Self {
        Self {
            inner,
            isolation_level: None,
            _phantom: std::marker::PhantomData,
        }
    }

    /// Configure the transaction isolation level for this node execution.
    pub fn with_isolation_level(mut self, isolation_level: IsolationLevel) -> Self {
        self.isolation_level = Some(isolation_level);
        self
    }

    /// Returns currently configured transaction isolation level.
    pub fn isolation_level(&self) -> Option<IsolationLevel> {
        self.isolation_level
    }
}

impl<T, From, To, R> Clone for TxPgNode<T, From, To, R>
where
    T: DbTransition<From, To> + Send + Sync + Clone,
    From: Send + Sync + 'static,
    To: Send + Sync + 'static,
    R: DbResources,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            isolation_level: self.isolation_level,
            _phantom: std::marker::PhantomData,
        }
    }
}

#[async_trait]
impl<T, From, To, R> Transition<From, To> for TxPgNode<T, From, To, R>
where
    T: DbTransition<From, To> + Send + Sync,
    From: Send + Sync + 'static,
    To: Send + Sync + 'static,
    R: DbResources,
{
    type Error = String;
    type Resources = R;

    async fn run(
        &self,
        input: From,
        resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<To, Self::Error> {
        let pool = resources.pg_pool();

        // Begin transaction
        let tx = match pool.begin().await {
            Ok(tx) => tx,
            Err(e) => {
                return Outcome::Fault(format!("Failed to begin transaction: {}", e));
            }
        };
        let mut tx = tx;

        if let Some(level) = self.isolation_level
            && let Err(e) = apply_postgres_isolation_level(&mut tx, level).await
        {
            return Outcome::Fault(format!(
                "Failed to set transaction isolation level: {}",
                e
            ));
        }

        // Execute the transition with the transaction
        if let Some(sql) = self.inner.sql_info() {
            tracing::debug!(ranvier.db.sql = %sql, "Executing transactional database operation");
        }

        let result = match self.inner.run(input, pool).await {
            Ok(result) => Outcome::Next(result),
            Err(DbError::NoRows) => {
                if let Err(e) = tx.rollback().await {
                    return Outcome::Fault(format!("Rollback failed: {}", e));
                }
                return Outcome::Fault("Record not found".to_string());
            }
            Err(e) => {
                if let Err(e) = tx.rollback().await {
                    return Outcome::Fault(format!("Rollback failed: {}", e));
                }
                return Outcome::Fault(format!("Database error: {}", e));
            }
        };

        // Commit on success
        if let Err(e) = tx.commit().await {
            return Outcome::Fault(format!("Commit failed: {}", e));
        }

        result
    }
}

async fn apply_postgres_isolation_level(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    level: IsolationLevel,
) -> Result<(), sqlx::Error> {
    sqlx::query(level.postgres_set_transaction_sql())
        .execute(&mut **tx)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct DummyDbTransition;

    #[async_trait]
    impl DbTransition<(), ()> for DummyDbTransition {
        type Error = anyhow::Error;

        async fn run(&self, _input: (), _pool: &sqlx::PgPool) -> QueryResult<()> {
            Ok(())
        }
    }

    #[derive(Clone)]
    struct DummyResources;

    impl ranvier_core::transition::ResourceRequirement for DummyResources {}

    impl DbResources for DummyResources {
        fn pg_pool(&self) -> &sqlx::PgPool {
            panic!("test-only resources should not call pg_pool")
        }
    }

    #[test]
    fn tx_node_accepts_isolation_level_configuration() {
        let node = TxPgNode::<DummyDbTransition, (), (), DummyResources>::new(DummyDbTransition)
            .with_isolation_level(IsolationLevel::Serializable);
        assert_eq!(node.isolation_level(), Some(IsolationLevel::Serializable));
    }

    #[test]
    fn isolation_level_generates_expected_postgres_sql() {
        assert_eq!(
            IsolationLevel::ReadUncommitted.postgres_set_transaction_sql(),
            "SET TRANSACTION ISOLATION LEVEL READ UNCOMMITTED"
        );
        assert_eq!(
            IsolationLevel::ReadCommitted.postgres_set_transaction_sql(),
            "SET TRANSACTION ISOLATION LEVEL READ COMMITTED"
        );
        assert_eq!(
            IsolationLevel::RepeatableRead.postgres_set_transaction_sql(),
            "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ"
        );
        assert_eq!(
            IsolationLevel::Serializable.postgres_set_transaction_sql(),
            "SET TRANSACTION ISOLATION LEVEL SERIALIZABLE"
        );
    }
}
