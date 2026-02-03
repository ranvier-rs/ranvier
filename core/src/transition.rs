//! # Transition: Typed State Transformation
//!
//! The `Transition` trait defines the contract for state transformations within a Decision Tree.
//!
//! ## Design Philosophy
//!
//! * **Explicit Input/Output**: Every transition declares `From` and `To` types
//! * **No Hidden Effects**: All effects must go through the `Bus`
//! * **Outcome-Based Control Flow**: Returns `Outcome` not `Result`

use crate::bus::Bus;
use crate::outcome::Outcome;
use async_trait::async_trait;

/// The contract for a Typed State Transition.
///
/// `Transition` converts state `From` to `Outcome<To, Error>`.
/// All transitions are async and receive access to the `Bus` for resource injection.
///
/// ## Example
///
/// ```rust
/// use async_trait::async_trait;
/// use ranvier_core::prelude::*;
///
/// # #[derive(Clone)]
/// # struct ValidateUser;
/// # #[async_trait::async_trait]
/// # impl Transition<String, String> for ValidateUser {
/// #     type Error = std::convert::Infallible;
/// #     async fn run(&self, input: String, _bus: &mut Bus) -> Outcome<String, Self::Error> {
/// #         Outcome::next(format!("validated: {}", input))
/// #     }
/// # }
/// #
/// # #[async_trait::async_trait]
/// # impl Transition<i32, i32> for DoubleValue {
/// #     type Error = std::convert::Infallible;
/// #     async fn run(&self, input: i32, _bus: &mut Bus) -> Outcome<i32, Self::Error> {
/// #         Outcome::next(input * 2)
/// #     }
/// # }
/// # struct DoubleValue;
/// ```
#[async_trait]
pub trait Transition<From, To>: Send + Sync + 'static
where
    From: Send + 'static,
    To: Send + 'static,
{
    /// Domain-specific error type (e.g., AuthError, ValidationError)
    type Error: Send + Sync + 'static;

    /// Execute the transition.
    ///
    /// # Parameters
    ///
    /// * `state` - The input state of type `From`
    /// * `bus` - Mutable reference to the resource Bus
    ///
    /// # Returns
    ///
    /// An `Outcome<To, Self::Error>` determining the next step:
    /// * `Outcome::Next(to)` - Continue to the next transition
    /// * `Outcome::Branch(id, payload)` - Branch to a named path
    /// * `Outcome::Jump(id, payload)` - Jump to a specific node
    /// * `Outcome::Emit(event, payload)` - Emit a side-effect event
    /// * `Outcome::Fault(err)` - Enter the error path
    async fn run(&self, state: From, bus: &mut Bus) -> Outcome<To, Self::Error>;
}

/// Blanket implementation for `Arc<T>` where `T: Transition`.
///
/// This allows sharing transitions across multiple Axons.
#[async_trait]
impl<T, From, To> Transition<From, To> for std::sync::Arc<T>
where
    T: Transition<From, To>,
    From: Send + 'static,
    To: Send + 'static,
{
    type Error = T::Error;

    async fn run(&self, state: From, bus: &mut Bus) -> Outcome<To, Self::Error> {
        self.as_ref().run(state, bus).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct AddOne;

    #[async_trait]
    impl Transition<i32, i32> for AddOne {
        type Error = std::convert::Infallible;

        async fn run(&self, state: i32, _bus: &mut Bus) -> Outcome<i32, Self::Error> {
            Outcome::Next(state + 1)
        }
    }

    #[tokio::test]
    async fn test_transition_basic() {
        let mut bus = Bus::new();
        let result = AddOne.run(41, &mut bus).await;
        assert!(matches!(result, Outcome::Next(42)));
    }
}
