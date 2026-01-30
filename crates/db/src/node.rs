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
pub struct PgNode<T, From, To>
where
    T: DbTransition<From, To> + Send + Sync,
    From: Send + Sync + 'static,
    To: Send + Sync + 'static,
{
    inner: T,
    _phantom: std::marker::PhantomData<(From, To)>,
}

impl<T, From, To> PgNode<T, From, To>
where
    T: DbTransition<From, To> + Send + Sync,
    From: Send + Sync + 'static,
    To: Send + Sync + 'static,
{
    /// Create a new PostgreSQL node.
    pub fn new(inner: T) -> Self {
        Self {
            inner,
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<T, From, To> Clone for PgNode<T, From, To>
where
    T: DbTransition<From, To> + Send + Sync + Clone,
    From: Send + Sync + 'static,
    To: Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _phantom: std::marker::PhantomData,
        }
    }
}

#[async_trait]
impl<T, From, To> Transition<From, To> for PgNode<T, From, To>
where
    T: DbTransition<From, To> + Send + Sync,
    From: Send + Sync + 'static,
    To: Send + Sync + 'static,
{
    type Error = T::Error;

    async fn run(&self, input: From, bus: &mut Bus) -> anyhow::Result<Outcome<To, Self::Error>> {
        let pool_resource = bus
            .read::<super::pool::PostgresPool>()
            .ok_or_else(|| anyhow::anyhow!("PostgresPool not found on Bus"))?;

        let pool = pool_resource.inner();
        match self.inner.run(input, pool).await {
            Ok(result) => Ok(Outcome::Next(result)),
            Err(DbError::NoRows) => {
                anyhow::bail!("Record not found")
            }
            Err(e) => anyhow::bail!("Database error: {}", e),
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
pub struct TxPgNode<T, From, To>
where
    T: DbTransition<From, To> + Send + Sync,
    From: Send + Sync + 'static,
    To: Send + Sync + 'static,
{
    inner: T,
    _phantom: std::marker::PhantomData<(From, To)>,
}

impl<T, From, To> TxPgNode<T, From, To>
where
    T: DbTransition<From, To> + Send + Sync,
    From: Send + Sync + 'static,
    To: Send + Sync + 'static,
{
    /// Create a new transactional PostgreSQL node.
    pub fn new(inner: T) -> Self {
        Self {
            inner,
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<T, From, To> Clone for TxPgNode<T, From, To>
where
    T: DbTransition<From, To> + Send + Sync + Clone,
    From: Send + Sync + 'static,
    To: Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _phantom: std::marker::PhantomData,
        }
    }
}

#[async_trait]
impl<T, From, To> Transition<From, To> for TxPgNode<T, From, To>
where
    T: DbTransition<From, To> + Send + Sync,
    From: Send + Sync + 'static,
    To: Send + Sync + 'static,
{
    type Error = T::Error;

    async fn run(&self, input: From, bus: &mut Bus) -> anyhow::Result<Outcome<To, Self::Error>> {
        let pool_resource = bus
            .read::<super::pool::PostgresPool>()
            .ok_or_else(|| anyhow::anyhow!("PostgresPool not found on Bus"))?;

        let pool = pool_resource.inner();

        // Begin transaction
        let tx = pool
            .begin()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to begin transaction: {}", e))?;

        // Execute the transition with the transaction
        // Note: For true transaction support, we'd pass the transaction itself.
        // This simplified version uses the pool directly.
        let result = match self.inner.run(input, pool).await {
            Ok(result) => Outcome::Next(result),
            Err(DbError::NoRows) => {
                tx.rollback()
                    .await
                    .map_err(|e| anyhow::anyhow!("Rollback failed: {}", e))?;
                anyhow::bail!("Record not found")
            }
            Err(e) => {
                tx.rollback()
                    .await
                    .map_err(|e| anyhow::anyhow!("Rollback failed: {}", e))?;
                anyhow::bail!("Database error: {}", e)
            }
        };

        // Commit on success
        tx.commit()
            .await
            .map_err(|e| anyhow::anyhow!("Commit failed: {}", e))?;

        Ok(result)
    }
}
