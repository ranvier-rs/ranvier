use crate::bus::Bus;
use crate::outcome::Outcome;
use async_trait::async_trait;

/// The contract for a Typed State Transition.
///
/// `Transition` converts state `From` to `Outcome<To, Error>`.
#[async_trait]
pub trait Transition<From, To>: Send + Sync + 'static
where
    From: Send + 'static,
    To: Send + 'static,
{
    /// Domain-specific error type (e.g., AuthError, ValidationError)
    type Error: Send + Sync + 'static;

    /// Execute the transition
    async fn run(&self, state: From, bus: &mut Bus) -> anyhow::Result<Outcome<To, Self::Error>>;
}
