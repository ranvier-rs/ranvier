use crate::bus::Bus;
use crate::outcome::Outcome;
use crate::schematic::{Edge, Node, NodeKind, Schematic};
use crate::transition::Transition;
use std::any::type_name;
use std::future::Future;
use std::pin::Pin;

pub type AxonResult<T, E> = anyhow::Result<Outcome<T, E>>;
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
pub type Executor<T, E> =
    Box<dyn for<'a> FnOnce(&'a mut Bus) -> BoxFuture<'a, AxonResult<T, E>> + Send>;

/// Helper to extract a readable type name from a type.
fn type_name_of<T: ?Sized>() -> String {
    let full = type_name::<T>();
    // Extract just the final identifier (e.g., "ValidationTransition" from "module::ValidationTransition")
    full.split("::").last().unwrap_or(full).to_string()
}

/// The Axon Builder and Runtime.
pub struct Axon<T, E> {
    pub schematic: Schematic,
    executor: Executor<T, E>,
}

impl<T: Send + 'static, E: Send + 'static> Axon<T, E> {
    /// Start a new Axon flow with an initial state literal.
    pub fn start(initial_state: T, label: &str) -> Self {
        let node_id = uuid::Uuid::new_v4().to_string();
        let node = Node {
            id: node_id,
            kind: NodeKind::Ingress,
            label: label.to_string(),
            input_type: "void".to_string(), // Start has no input really, or we could say Void
            output_type: type_name_of::<T>(),
            metadata: Default::default(),
        };

        let mut schematic = Schematic::new(label);
        schematic.nodes.push(node);

        let executor: Executor<T, E> = Box::new(move |_bus: &mut Bus| {
            Box::pin(async move { Ok(Outcome::Next(initial_state)) })
                as BoxFuture<'_, AxonResult<T, E>>
        });

        Self {
            schematic,
            executor,
        }
    }

    /// Chain a transition.
    /// This consumes the current Axon and returns a new Axon with the next state.
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
            kind: crate::schematic::EdgeType::Linear,
            label: Some("Next".to_string()),
        });

        // Update Executor
        let prev_executor = self.executor;
        let next_executor: Executor<Next, E> = Box::new(move |bus: &mut Bus| {
            Box::pin(async move {
                // Run previous step
                // Reborrow bus to avoid moving the reference into prev_executor
                let prev_result = prev_executor(&mut *bus).await?;

                // Unpack the state from Outcome, preserving control flow
                let state = match prev_result {
                    Outcome::Next(t) => t,
                    // If the previous step branched, jumped, or emitted a signal,
                    // we strictly propagate it and DO NOT execute this step.
                    Outcome::Branch(id, payload) => return Ok(Outcome::Branch(id, payload)),
                    Outcome::Jump(id, payload) => return Ok(Outcome::Jump(id, payload)),
                    Outcome::Emit(evt, payload) => return Ok(Outcome::Emit(evt, payload)),
                    Outcome::Fault(e) => return Ok(Outcome::Fault(e)),
                };

                // Execute Transition
                transition.run(state, bus).await
            }) as BoxFuture<'_, AxonResult<Next, E>>
        });

        Axon {
            schematic: self.schematic,
            executor: next_executor,
        }
    }

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

    /// Execute the Axon.
    pub async fn execute(self, bus: &mut Bus) -> AxonResult<T, E> {
        (self.executor)(bus).await
    }
}
