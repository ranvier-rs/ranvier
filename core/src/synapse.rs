use async_trait::async_trait;

/// Synapse: The Integration Layer
///
/// A Synapse represents a connection to an external system or side-effect.
/// It creates a standard interface for I/O operations.
#[async_trait]
pub trait Synapse: Send + Sync {
    type Input: Send;
    type Output: Send;
    type Error: std::fmt::Debug + Send;

    /// Executes the integration logic (e.g., DB query, API call)
    async fn call(&self, input: Self::Input) -> Result<Self::Output, Self::Error>;
}
