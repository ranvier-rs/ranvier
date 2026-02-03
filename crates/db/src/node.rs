//! Database-Aware Node Types for Ranvier Axon
//!
//! Provides `DbNode` and `DbTransition` traits for database operations
//! that integrate with the Axon execution model.

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
    type Error = anyhow::Error;
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
            Err(DbError::NoRows) => Outcome::Fault(anyhow::anyhow!("Record not found")),
            Err(e) => Outcome::Fault(anyhow::anyhow!("Database error: {}", e)),
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
            _phantom: std::marker::PhantomData,
        }
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
    type Error = anyhow::Error;
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
                return Outcome::Fault(anyhow::anyhow!("Failed to begin transaction: {}", e));
            }
        };

        // Execute the transition with the transaction
        if let Some(sql) = self.inner.sql_info() {
            tracing::debug!(ranvier.db.sql = %sql, "Executing transactional database operation");
        }

        let result = match self.inner.run(input, pool).await {
            Ok(result) => Outcome::Next(result),
            Err(DbError::NoRows) => {
                if let Err(e) = tx.rollback().await {
                    return Outcome::Fault(anyhow::anyhow!("Rollback failed: {}", e));
                }
                return Outcome::Fault(anyhow::anyhow!("Record not found"));
            }
            Err(e) => {
                if let Err(e) = tx.rollback().await {
                    return Outcome::Fault(anyhow::anyhow!("Rollback failed: {}", e));
                }
                return Outcome::Fault(anyhow::anyhow!("Database error: {}", e));
            }
        };

        // Commit on success
        if let Err(e) = tx.commit().await {
            return Outcome::Fault(anyhow::anyhow!("Commit failed: {}", e));
        }

        result
    }
}
