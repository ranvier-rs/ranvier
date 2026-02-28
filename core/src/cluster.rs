use async_trait::async_trait;
use thiserror::Error;

/// Error type for distributed cluster operations
#[derive(Debug, Error)]
pub enum ClusterError {
    #[error("Failed to acquire lock: {0}")]
    LockAcquisitionFailed(String),
    #[error("Lock is already held by another node: {0}")]
    LockHeld(String),
    #[error("Failed to release lock: {0}")]
    LockReleaseFailed(String),
    #[error("Cluster bus error: {0}")]
    BusError(String),
    #[error("Connection error: {0}")]
    ConnectionError(String),
    #[error("Internal cluster error: {0}")]
    Internal(String),
}

/// Interface for distributed locking mechanisms.
/// Ensures that scheduled tasks or singleton operations run exactly once across the cluster.
#[async_trait]
pub trait DistributedLock: Send + Sync {
    /// Attempts to acquire a distributed lock.
    ///
    /// # Arguments
    /// * `key` - The unique identifier for the lock.
    /// * `ttl_ms` - Time-to-live in milliseconds before the lock automatically expires.
    ///
    /// # Returns
    /// * `Ok(true)` if the lock was successfully acquired.
    /// * `Ok(false)` if the lock is currently held by another node.
    /// * `Err` if a cluster or connection error occurred.
    async fn try_acquire(&self, key: &str, ttl_ms: u64) -> Result<bool, ClusterError>;

    /// Releases a previously acquired distributed lock.
    ///
    /// # Arguments
    /// * `key` - The unique identifier for the lock.
    async fn release(&self, key: &str) -> Result<(), ClusterError>;

    /// Extends the expiration time of an actively held lock.
    ///
    /// # Arguments
    /// * `key` - The unique identifier for the lock.
    /// * `extra_ttl_ms` - Additional time-to-live to add to the lock.
    async fn extend(&self, key: &str, extra_ttl_ms: u64) -> Result<(), ClusterError>;
}

/// Interface for a distributed message bus.
/// Facilitates inter-node coordination, such as state synchronization or cluster-wide events.
#[async_trait]
pub trait ClusterBus: Send + Sync {
    /// Publishes a message to a specific cluster topic.
    async fn publish(&self, topic: &str, payload: &[u8]) -> Result<(), ClusterError>;

    /// Subscribes to a specific cluster topic, returning a stream or receiver of messages.
    /// The exact return type is abstracted or wrapped depending on implementation.
    /// For this trait definition, we represent the registration of intent.
    async fn subscribe(&self, topic: &str) -> Result<(), ClusterError>;
}
