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
use std::fmt::Debug;

/// Resource requirement for a transition.
///
/// This trait is used to mark types that can be injected as resources.
/// Implementations should usually be a struct representing a bundle of resources.
pub trait ResourceRequirement: Send + Sync + 'static {}

/// Blanket implementation for () if no resources are needed.
impl ResourceRequirement for () {}

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
    type Error: Send + Sync + Debug + 'static;

    /// The type of resources required by this transition.
    /// This follows the "Hard-Wired Types" principle from the Master Plan.
    type Resources: ResourceRequirement;

    /// Execute the transition.
    ///
    /// # Parameters
    ///
    /// * `state` - The input state of type `From`
    /// * `resources` - Typed access to required resources
    /// * `bus` - The base Bus (for cross-cutting concerns like telemetry)
    ///
    /// # Returns
    ///
    /// An `Outcome<To, Self::Error>` determining the next step.
    /// Returns a human-readable label for this transition.
    /// Defaults to the type name.
    fn label(&self) -> String {
        let full = std::any::type_name::<Self>();
        full.split("::").last().unwrap_or(full).to_string()
    }

    /// Returns a detailed description of what this transition does.
    fn description(&self) -> Option<String> {
        None
    }

    /// Execute the transition.
    ///
    /// # Parameters
    ///
    /// * `state` - The input state of type `From`
    /// * `resources` - Typed access to required resources
    /// * `bus` - The base Bus (for cross-cutting concerns like telemetry)
    ///
    /// # Returns
    ///
    /// An `Outcome<To, Self::Error>` determining the next step.
    async fn run(
        &self,
        state: From,
        resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<To, Self::Error>;
}

/// Blanket implementation for `Arc<T>` where `T: Transition`.
///
/// This allows sharing transitions across multiple Axons.
#[async_trait]
impl<T, From, To> Transition<From, To> for std::sync::Arc<T>
where
    T: Transition<From, To> + Send + Sync + 'static,
    From: Send + 'static,
    To: Send + 'static,
{
    type Error = T::Error;
    type Resources = T::Resources;

    async fn run(
        &self,
        state: From,
        resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<To, Self::Error> {
        self.as_ref().run(state, resources, bus).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct AddOne;

    #[async_trait]
    impl Transition<i32, i32> for AddOne {
        type Error = std::convert::Infallible;
        type Resources = ();

        async fn run(
            &self,
            state: i32,
            _resources: &Self::Resources,
            _bus: &mut Bus,
        ) -> Outcome<i32, Self::Error> {
            Outcome::Next(state + 1)
        }
    }

    #[tokio::test]
    async fn test_transition_basic() {
        let mut bus = Bus::new();
        let result = AddOne.run(41, &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(42)));
    }
}
