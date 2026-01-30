//! Database Connection Pool Support for Ranvier
//!
//! Provides typed connection pools that can be stored on the `Bus` as resources.

use async_trait::async_trait;

/// Generic database pool trait.
///
/// `DbPool` abstracts over different database backends (PostgreSQL, MySQL, SQLite).
/// Pools are stored as Resources on the `Bus`.
#[async_trait]
pub trait DbPool: Send + Sync + 'static {
    /// The raw connection type for this database.
    type Connection: Send + 'static;

    /// Get a connection from the pool.
    async fn acquire(&self) -> Result<Self::Connection, DbPoolError>;

    /// Check if the pool is healthy.
    async fn ping(&self) -> Result<(), DbPoolError>;

    /// Get the pool size configuration.
    fn size(&self) -> PoolSize;

    /// Close the pool gracefully.
    async fn close(&self) -> Result<(), DbPoolError>;
}

/// Pool size configuration.
#[derive(Debug, Clone, Copy)]
pub struct PoolSize {
    /// Maximum number of connections.
    pub max: u32,
    /// Minimum number of connections (idle).
    pub min: u32,
    /// Current number of active connections.
    pub active: u32,
}

/// Pool-related errors.
#[derive(Debug, thiserror::Error)]
pub enum DbPoolError {
    #[error("Failed to acquire connection from pool: {0}")]
    AcquireFailed(String),

    #[error("Pool is closed")]
    PoolClosed,

    #[error("Connection timed out")]
    Timeout,

    #[error("Backend error: {0}")]
    Backend(String),

    #[error("Pool configuration error: {0}")]
    Configuration(String),
}

// ============== PostgreSQL Pool ==============

/// PostgreSQL connection pool wrapper.
///
/// This type can be stored on the `Bus` as a resource.
/// Example:
/// ```rust
/// let pool = PostgresPool::new("postgres://user:pass@localhost/db").await?;
/// bus.write(pool);
/// ```
#[derive(Clone)]
pub struct PostgresPool {
    inner: sqlx::PgPool,
}

impl PostgresPool {
    /// Create a new PostgreSQL pool from a connection string.
    pub async fn new(url: &str) -> Result<Self, DbPoolError> {
        Self::with_options(url, sqlx::postgres::PgPoolOptions::new()).await
    }

    /// Create a pool with custom options.
    pub async fn with_options(
        url: &str,
        options: sqlx::postgres::PgPoolOptions,
    ) -> Result<Self, DbPoolError> {
        let pool = options
            .connect(url)
            .await
            .map_err(|e| DbPoolError::Backend(e.to_string()))?;
        Ok(Self { inner: pool })
    }

    /// Get the underlying `sqlx::PgPool`.
    pub fn inner(&self) -> &sqlx::PgPool {
        &self.inner
    }
}

#[async_trait]
impl DbPool for PostgresPool {
    type Connection = sqlx::pool::PoolConnection<sqlx::Postgres>;

    async fn acquire(&self) -> Result<Self::Connection, DbPoolError> {
        self.inner
            .acquire()
            .await
            .map_err(|e| DbPoolError::AcquireFailed(e.to_string()))
    }

    async fn ping(&self) -> Result<(), DbPoolError> {
        // Execute a simple query to verify connection
        sqlx::query("SELECT 1")
            .fetch_one(self.inner())
            .await
            .map_err(|e| DbPoolError::Backend(e.to_string()))?;
        Ok(())
    }

    fn size(&self) -> PoolSize {
        PoolSize {
            max: self.inner.size(),
            min: self.inner.num_idle() as u32,
            active: self.inner.num_idle() as u32,
        }
    }

    async fn close(&self) -> Result<(), DbPoolError> {
        self.inner.close().await;
        Ok(())
    }
}

// ============== MySQL Pool ==============

/// MySQL connection pool wrapper.
#[derive(Clone)]
pub struct MySqlPool {
    inner: sqlx::MySqlPool,
}

impl MySqlPool {
    /// Create a new MySQL pool from a connection string.
    pub async fn new(url: &str) -> Result<Self, DbPoolError> {
        let pool = sqlx::mysql::MySqlPoolOptions::new()
            .connect(url)
            .await
            .map_err(|e| DbPoolError::Backend(e.to_string()))?;
        Ok(Self { inner: pool })
    }

    /// Get the underlying `sqlx::MySqlPool`.
    pub fn inner(&self) -> &sqlx::MySqlPool {
        &self.inner
    }
}

#[async_trait]
impl DbPool for MySqlPool {
    type Connection = sqlx::pool::PoolConnection<sqlx::MySql>;

    async fn acquire(&self) -> Result<Self::Connection, DbPoolError> {
        self.inner
            .acquire()
            .await
            .map_err(|e| DbPoolError::AcquireFailed(e.to_string()))
    }

    async fn ping(&self) -> Result<(), DbPoolError> {
        // Execute a simple query to verify connection
        sqlx::query("SELECT 1")
            .fetch_one(self.inner())
            .await
            .map_err(|e| DbPoolError::Backend(e.to_string()))?;
        Ok(())
    }

    fn size(&self) -> PoolSize {
        PoolSize {
            max: self.inner.size(),
            min: self.inner.num_idle() as u32,
            active: self.inner.num_idle() as u32,
        }
    }

    async fn close(&self) -> Result<(), DbPoolError> {
        self.inner.close().await;
        Ok(())
    }
}

// ============== SQLite Pool ==============

/// SQLite connection pool wrapper.
#[derive(Clone)]
pub struct SqlitePool {
    inner: sqlx::SqlitePool,
}

impl SqlitePool {
    /// Create a new SQLite pool from a connection string.
    pub async fn new(url: &str) -> Result<Self, DbPoolError> {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect(url)
            .await
            .map_err(|e| DbPoolError::Backend(e.to_string()))?;
        Ok(Self { inner: pool })
    }

    /// Get the underlying `sqlx::SqlitePool`.
    pub fn inner(&self) -> &sqlx::SqlitePool {
        &self.inner
    }
}

#[async_trait]
impl DbPool for SqlitePool {
    type Connection = sqlx::pool::PoolConnection<sqlx::Sqlite>;

    async fn acquire(&self) -> Result<Self::Connection, DbPoolError> {
        self.inner
            .acquire()
            .await
            .map_err(|e| DbPoolError::AcquireFailed(e.to_string()))
    }

    async fn ping(&self) -> Result<(), DbPoolError> {
        // SQLite doesn't have a ping method, so we execute a simple query
        sqlx::query("SELECT 1")
            .fetch_one(self.inner())
            .await
            .map_err(|e| DbPoolError::Backend(e.to_string()))?;
        Ok(())
    }

    fn size(&self) -> PoolSize {
        PoolSize {
            max: self.inner.size(),
            min: self.inner.num_idle() as u32,
            active: self.inner.num_idle() as u32,
        }
    }

    async fn close(&self) -> Result<(), DbPoolError> {
        self.inner.close().await;
        Ok(())
    }
}

// Note: PostgresPool, MySqlPool, and SqlitePool already implement Any + Send + Sync
// through their field types, so they can be stored on the Bus as resources.
