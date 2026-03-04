use async_trait::async_trait;
use std::fmt;

/// Errors that can occur during distributed state operations.
#[derive(Debug)]
pub enum DistributedError {
    StoreError(String),
    LockError(String),
    NotFound(String),
}

impl fmt::Display for DistributedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StoreError(e) => write!(f, "Store error: {}", e),
            Self::LockError(e) => write!(f, "Lock error: {}", e),
            Self::NotFound(e) => write!(f, "Not found: {}", e),
        }
    }
}

impl std::error::Error for DistributedError {}

/// An abstraction for a distributed key-value store.
#[async_trait]
pub trait DistributedStore: Send + Sync {
    /// Retrieve a value from the store.
    async fn get(&self, domain: &str, key: &str) -> Result<Option<Vec<u8>>, DistributedError>;

    /// Set a value in the store, optionally with a time-to-live (in seconds).
    async fn set(
        &self,
        domain: &str,
        key: &str,
        value: &[u8],
        ttl_sec: Option<u64>,
    ) -> Result<(), DistributedError>;

    /// Delete a value from the store.
    async fn delete(&self, domain: &str, key: &str) -> Result<(), DistributedError>;
}

/// Options to attempt to acquire a distributed lock.
#[derive(Debug, Clone)]
pub struct LockOptions {
    /// Time-to-live for the lock in milliseconds.
    pub ttl_ms: u64,
    /// Number of attempts to acquire the lock.
    pub retry_count: u32,
    /// Delay between retry attempts in milliseconds.
    pub retry_delay_ms: u64,
}

impl Default for LockOptions {
    fn default() -> Self {
        Self {
            ttl_ms: 10_000,
            retry_count: 5,
            retry_delay_ms: 200,
        }
    }
}

/// Representation of an acquired lock.
#[derive(Debug, Clone)]
pub struct Guard {
    /// The unique resource identifier for this lock.
    pub resource_key: String,
    /// The specific token given to the lock owner.
    pub token: String,
}

/// An abstraction for acquiring and releasing distributed locks.
#[async_trait]
pub trait DistributedLock: Send + Sync {
    /// Attempt to acquire a distributed lock on a resource.
    async fn acquire(
        &self,
        resource_key: &str,
        options: LockOptions,
    ) -> Result<Guard, DistributedError>;

    /// Release the acquired lock.
    async fn release(&self, guard: Guard) -> Result<(), DistributedError>;

    /// Extend the TTL of the currently held lock.
    async fn extend(&self, guard: &Guard, additional_ttl_ms: u64) -> Result<(), DistributedError>;
}
