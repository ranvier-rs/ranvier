//! # Axon: Executable Decision Tree
//!
//! The `Axon` is the **runtime execution path** of a Typed Decision Tree.
//!
//! ## Design Philosophy
//!
//! * **Axon flows, Schematic shows**: Axon executes; Schematic describes
//! * **Builder pattern**: `Axon::start().then().then()`
//! * **Schematic extraction**: Every Axon carries its structural metadata
//!
//! "Axon is the flowing thing, Schematic is the visible thing."

use crate::bus::Bus;
use crate::outcome::Outcome;
use crate::schematic::{Edge, EdgeType, Node, NodeKind, Schematic};
use crate::transition::Transition;
use std::any::type_name;
use std::future::Future;
use std::pin::Pin;

/// Type alias for async boxed futures used in Axon execution.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Executor type for Axon steps.
pub type Executor<T, E> =
    Box<dyn for<'a> FnOnce(&'a mut Bus) -> BoxFuture<'a, Outcome<T, E>> + Send>;

/// Helper to extract a readable type name from a type.
fn type_name_of<T: ?Sized>() -> String {
    let full = type_name::<T>();
    // Extract just the final identifier (e.g., "ValidationTransition" from "module::ValidationTransition")
    full.split("::").last().unwrap_or(full).to_string()
}

/// The Axon Builder and Runtime.
///
/// `Axon` represents an executable decision tree. It builds execution chains
/// using the builder pattern and simultaneously maintains a `Schematic`
/// for visualization and analysis.
///
/// ## Example
///
/// ```rust
/// use ranvier_core::prelude::*;
/// use async_trait::async_trait;
///
/// # #[derive(Clone)]
/// # struct StepA;
/// # #[async_trait::async_trait]
/// # impl Transition<(), i32> for StepA {
/// #     type Error = std::convert::Infallible;
/// #     async fn run(&self, _: (), _: &mut Bus) -> Outcome<i32, Self::Error> {
/// #         Outcome::Next(42)
/// #     }
/// # }
///
/// # #[derive(Clone)]
/// # struct StepB;
/// # #[async_trait::async_trait]
/// # impl Transition<i32, i32> for StepB {
/// #     type Error = std::convert::Infallible;
/// #     async fn run(&self, input: i32, _: &mut Bus) -> Outcome<i32, Self::Error> {
/// #         Outcome::Next(input + 1)
/// #     }
/// # }
///
/// # async fn example() {
/// let axon = Axon::start((), "My Axon")
///     .then(StepA)
///     .then(StepB);
///
/// let mut bus = Bus::new();
/// let result = axon.execute(&mut bus).await;
/// # }
/// ```
pub struct Axon<T, E> {
    /// The static structure (for visualization/analysis)
    pub schematic: Schematic,
    /// The runtime executor
    executor: Executor<T, E>,
}

impl<T: Send + 'static, E: Send + 'static> Axon<T, E> {
    /// Start a new Axon flow with an initial state literal.
    ///
    /// # Parameters
    ///
    /// * `initial_state` - The starting state value
    /// * `label` - A descriptive label for this Axon (appears in Schematic)
    pub fn start(initial_state: T, label: &str) -> Self {
        let node_id = uuid::Uuid::new_v4().to_string();
        let node = Node {
            id: node_id,
            kind: NodeKind::Ingress,
            label: label.to_string(),
            input_type: "void".to_string(), // Start has no input
            output_type: type_name_of::<T>(),
            metadata: Default::default(),
            source_location: None,
        };

        let mut schematic = Schematic::new(label);
        schematic.nodes.push(node);

        let executor: Executor<T, E> = Box::new(move |_bus: &mut Bus| {
            Box::pin(async move { Outcome::Next(initial_state) }) as BoxFuture<'_, Outcome<T, E>>
        });

        Self {
            schematic,
            executor,
        }
    }

    /// Chain a transition to this Axon.
    ///
    /// This consumes the current Axon and returns a new Axon with the next state.
    ///
    /// # Parameters
    ///
    /// * `transition` - A `Transition<From, To>` implementation
    ///
    /// # Type Parameters
    ///
    /// * `Next` - The output state type of the transition
    /// * `Trans` - The transition type (must implement `Transition<T, Next>`)
    pub fn then<Next, Trans>(mut self, transition: Trans) -> Axon<Next, E>
    where
        Next: Send + 'static,
        Trans: Transition<T, Next, Error = E> + Clone + Send + Sync + 'static,
    {
        // Extract readable type name for the transition
        let trans_label = type_name_of::<Trans>();

        // Update Schematic
        let next_node_id = uuid::Uuid::new_v4().to_string();
        let next_node = Node {
            id: next_node_id.clone(),
            kind: NodeKind::Atom,
            label: trans_label,
            input_type: type_name_of::<T>(),
            output_type: type_name_of::<Next>(),
            metadata: Default::default(),
            source_location: None,
        };
        // Edge from last node to this
        let last_node_id = self
            .schematic
            .nodes
            .last()
            .map(|n| n.id.clone())
            .unwrap_or_default();

        self.schematic.nodes.push(next_node);
        self.schematic.edges.push(Edge {
            from: last_node_id,
            to: next_node_id,
            kind: EdgeType::Linear,
            label: Some("Next".to_string()),
        });

        // Update Executor
        let prev_executor = self.executor;
        let next_executor: Executor<Next, E> = Box::new(move |bus: &mut Bus| {
            Box::pin(async move {
                // Run previous step
                // Reborrow bus to avoid moving the reference into prev_executor
                let prev_result = prev_executor(&mut *bus).await;

                // Unpack the state from Outcome, preserving control flow
                let state = match prev_result {
                    Outcome::Next(t) => t,
                    // If the previous step branched, jumped, or emitted a signal,
                    // we strictly propagate it and DO NOT execute this step.
                    Outcome::Branch(id, payload) => return Outcome::Branch(id, payload),
                    Outcome::Jump(id, payload) => return Outcome::Jump(id, payload),
                    Outcome::Emit(evt, payload) => return Outcome::Emit(evt, payload),
                    Outcome::Fault(e) => return Outcome::Fault(e),
                };

                // Execute Transition
                transition.run(state, bus).await
            }) as BoxFuture<'_, Outcome<Next, E>>
        });

        Axon {
            schematic: self.schematic,
            executor: next_executor,
        }
    }

    /// Add a branch point to this Axon.
    ///
    /// Creates a conditional branch that can be triggered by `Outcome::Branch`.
    ///
    /// # Parameters
    ///
    /// * `branch_id` - The identifier for this branch
    /// * `label` - A descriptive label for the branch node
    pub fn branch(mut self, branch_id: impl Into<String>, label: &str) -> Self {
        let branch_id_str = branch_id.into();
        let last_node_id = self
            .schematic
            .nodes
            .last()
            .map(|n| n.id.clone())
            .unwrap_or_default();

        let branch_node = Node {
            id: uuid::Uuid::new_v4().to_string(),
            kind: NodeKind::Synapse,
            label: label.to_string(),
            input_type: type_name_of::<T>(),
            output_type: type_name_of::<T>(),
            metadata: Default::default(),
            source_location: None,
        };

        self.schematic.nodes.push(branch_node);
        self.schematic.edges.push(Edge {
            from: last_node_id,
            to: branch_id_str.clone(),
            kind: EdgeType::Branch(branch_id_str),
            label: Some("Branch".to_string()),
        });

        self
    }

    /// Execute the Axon.
    ///
    /// Runs the entire execution chain and returns the final `Outcome`.
    ///
    /// # Parameters
    ///
    /// * `bus` - Mutable reference to the resource Bus
    ///
    /// # Returns
    ///
    /// The final `Outcome<T, E>` from the execution chain.
    pub async fn execute(self, bus: &mut Bus) -> Outcome<T, E> {
        (self.executor)(bus).await
    }

    /// Get a reference to the Schematic (structural view).
    pub fn schematic(&self) -> &Schematic {
        &self.schematic
    }

    /// Consume and return the Schematic.
    pub fn into_schematic(self) -> Schematic {
        self.schematic
    }
}

impl<T, E> Axon<T, E> {
    /// Helper to unpack state from an Outcome, handling early returns.
    /// Returns None if the Outcome is not Next.
    pub fn unpack_state(outcome: Outcome<T, E>) -> Option<T> {
        match outcome {
            Outcome::Next(t) => Some(t),
            _ => None,
        }
    }

    /// Check if an outcome represents a fault/error.
    pub fn is_fault(outcome: &Outcome<T, E>) -> bool {
        matches!(outcome, Outcome::Fault(_))
    }

    /// Check if an outcome represents a branch.
    pub fn is_branch(outcome: &Outcome<T, E>) -> bool {
        matches!(outcome, Outcome::Branch(_, _))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    #[derive(Clone)]
    struct Inc;

    #[async_trait]
    impl Transition<i32, i32> for Inc {
        type Error = std::convert::Infallible;

        async fn run(&self, state: i32, _bus: &mut Bus) -> Outcome<i32, Self::Error> {
            Outcome::Next(state + 1)
        }
    }

    #[tokio::test]
    async fn test_axon_builder() {
        let axon = Axon::start(0, "Test").then(Inc).then(Inc);
        assert_eq!(axon.schematic.nodes.len(), 3); // Start + 2 transitions
    }

    #[tokio::test]
    async fn test_axon_execution() {
        let axon = Axon::start(0, "Test").then(Inc).then(Inc);
        let mut bus = Bus::new();
        let result = axon.execute(&mut bus).await;
        assert!(matches!(result, Outcome::Next(2)));
    }
}
