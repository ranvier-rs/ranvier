//! # Axon: Executable Decision Tree
//!
//! The `Axon` is the **runtime execution path** of a Typed Decision Tree.
//! It functions as a reusable Pipeline<In, Out>.
//!
//! ## Design Philosophy
//!
//! * **Axon flows, Schematic shows**: Axon executes; Schematic describes
//! * **Builder pattern**: `Axon::start().then().then()`
//! * **Schematic extraction**: Every Axon carries its structural metadata
//!
//! "Axon is the flowing thing, Schematic is the visible thing."

use ranvier_core::bus::Bus;
use ranvier_core::outcome::Outcome;
use ranvier_core::schematic::{Edge, EdgeType, Node, NodeKind, Schematic};
use ranvier_core::transition::Transition;
use std::any::type_name;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tracing::Instrument;

/// Type alias for async boxed futures used in Axon execution.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Executor type for Axon steps.
/// Now takes an input state `In` and returns an `Outcome<Out, E>`.
/// Must be Send + Sync to be reusable across threads and clones.
pub type Executor<In, Out, E> =
    Arc<dyn for<'a> Fn(In, &'a mut Bus) -> BoxFuture<'a, Outcome<Out, E>> + Send + Sync>;

/// Helper to extract a readable type name from a type.
fn type_name_of<T: ?Sized>() -> String {
    let full = type_name::<T>();
    full.split("::").last().unwrap_or(full).to_string()
}

/// The Axon Builder and Runtime.
///
/// `Axon` represents an executable decision tree (Pipeline).
/// It is reusable and thread-safe.
///
/// ## Example
///
/// ```rust,ignore
/// use ranvier_core::prelude::*;
/// // ...
/// // Start with an identity Axon (In -> In)
/// let axon = Axon::<(), (), _>::new("My Axon")
///     .then(StepA)
///     .then(StepB);
///
/// // Execute multiple times
/// let res1 = axon.execute((), &mut bus1).await;
/// let res2 = axon.execute((), &mut bus2).await;
/// ```
pub struct Axon<In, Out, E> {
    /// The static structure (for visualization/analysis)
    pub schematic: Schematic,
    /// The runtime executor
    executor: Executor<In, Out, E>,
}

impl<In, Out, E> Clone for Axon<In, Out, E> {
    fn clone(&self) -> Self {
        Self {
            schematic: self.schematic.clone(),
            executor: self.executor.clone(),
        }
    }
}

impl<In, E> Axon<In, In, E>
where
    In: Send + Sync + 'static,
    E: Send + 'static,
{
    /// Create a new Axon flow with the given label.
    /// This is the preferred entry point per Flat API guidelines.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let hello = Axon::new("HelloWorld")
    ///     .then(GreetBlock)
    ///     .then(ExclaimBlock);
    /// ```
    pub fn new(label: &str) -> Self {
        Self::start(label)
    }

    /// Start defining a new Axon flow.
    /// This creates an Identity Axon (In -> In).
    pub fn start(label: &str) -> Self {
        let node_id = uuid::Uuid::new_v4().to_string();
        let node = Node {
            id: node_id,
            kind: NodeKind::Ingress,
            label: label.to_string(),
            input_type: "void".to_string(), // Ingress usually starts from void/external
            output_type: type_name_of::<In>(),
            metadata: Default::default(),
            source_location: None,
        };

        let mut schematic = Schematic::new(label);
        schematic.nodes.push(node);

        // The initial executor is just an identity function matching the In type
        let executor: Executor<In, In, E> = Arc::new(
            move |input: In, _bus: &mut Bus| -> BoxFuture<'_, Outcome<In, E>> {
                Box::pin(std::future::ready(Outcome::Next(input)))
            },
        );

        Self {
            schematic,
            executor,
        }
    }
}

impl<In, Out, E> Axon<In, Out, E>
where
    In: Send + Sync + 'static,
    Out: Send + Sync + 'static,
    E: Send + 'static,
{
    /// Chain a transition to this Axon.
    pub fn then<Next, Trans>(self, transition: Trans) -> Axon<In, Next, E>
    where
        Next: Send + Sync + 'static,
        Trans: Transition<Out, Next, Error = E> + Clone + Send + Sync + 'static,
    {
        let trans_label = type_name_of::<Trans>();

        // Decompose self to avoid partial move issues
        let Axon {
            mut schematic,
            executor: prev_executor,
        } = self;

        // Update Schematic
        let next_node_id = uuid::Uuid::new_v4().to_string();
        let next_node = Node {
            id: next_node_id.clone(),
            kind: NodeKind::Atom,
            label: trans_label.clone(),
            input_type: type_name_of::<Out>(),
            output_type: type_name_of::<Next>(),
            metadata: Default::default(),
            source_location: None,
        };

        let last_node_id = schematic
            .nodes
            .last()
            .map(|n| n.id.clone())
            .unwrap_or_default();

        schematic.nodes.push(next_node);
        schematic.edges.push(Edge {
            from: last_node_id,
            to: next_node_id,
            kind: EdgeType::Linear,
            label: Some("Next".to_string()),
        });

        // Compose Executor
        let next_executor: Executor<In, Next, E> = Arc::new(
            move |input: In, bus: &mut Bus| -> BoxFuture<'_, Outcome<Next, E>> {
                let prev = prev_executor.clone();
                let trans = transition.clone();
                let _label = trans_label.clone();

                Box::pin(async move {
                    // Run previous step
                    let prev_result = prev(input, bus).await;

                    // Unpack
                    let state = match prev_result {
                        Outcome::Next(t) => t,
                        Outcome::Branch(id, p) => return Outcome::Branch(id, p),
                        Outcome::Jump(id, p) => return Outcome::Jump(id, p),
                        Outcome::Emit(evt, p) => return Outcome::Emit(evt, p),
                        Outcome::Fault(e) => return Outcome::Fault(e),
                    };

                    // Run this step
                    async move { trans.run(state, bus).await }.await
                })
            },
        );

        Axon {
            schematic,
            executor: next_executor,
        }
    }

    /// Add a branch point
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
            input_type: type_name_of::<Out>(),
            output_type: type_name_of::<Out>(),
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

    /// Execute the Axon with the given input.
    pub async fn execute(&self, input: In, bus: &mut Bus) -> Outcome<Out, E> {
        let label = self.schematic.name.clone();
        async move { (self.executor)(input, bus).await }
            .instrument(tracing::info_span!("Circuit", ranvier.circuit = %label))
            .await
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
