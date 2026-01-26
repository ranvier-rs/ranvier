//! AsyncTransition - Async State Transition
//!
//! This is the async version of `Transition` from the flow layer.
//! Used for transitions that require async operations (DB, HTTP, etc.)

use async_trait::async_trait;

/// Async version of the Transition trait.
///
/// Use this when your state transition requires async operations
/// such as database queries, HTTP calls, or file I/O.
#[async_trait]
pub trait AsyncTransition<From, To>: Send + Sync {
    /// Error type for this transition
    type Error: Send;

    /// Context type (Bus) for resource access
    type Context: Send + Sync;

    /// Perform the async state transition
    async fn transition(from: From, ctx: &Self::Context) -> Result<To, Self::Error>;
}

/// Async branching transition
#[async_trait]
pub trait AsyncBranchTransition<From>: Send + Sync {
    /// The output enum representing all possible branches
    type Output: Send;

    /// Error type
    type Error: Send;

    /// Context type
    type Context: Send + Sync;

    /// Perform the async branching transition
    async fn branch(from: From, ctx: &Self::Context) -> Result<Self::Output, Self::Error>;
}
