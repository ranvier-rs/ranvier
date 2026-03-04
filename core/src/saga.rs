use crate::bus::Bus;
use crate::outcome::Outcome;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// A type-erased compensation handler for Saga rollbacks.
///
/// Takes serialized input snapshot, resources, and bus.
pub type SagaCompensationFn<E, Res> = Arc<
    dyn for<'a> Fn(Vec<u8>, &'a Res, &'a mut Bus) -> BoxFuture<'a, Outcome<(), E>> + Send + Sync,
>;

/// Registry for Saga compensation handlers, keyed by node ID.
#[derive(Clone, Default)]
pub struct SagaCompensationRegistry<E, Res> {
    /// Maps node ID to its compensation handler.
    pub handlers: HashMap<String, SagaCompensationFn<E, Res>>,
}

impl<E, Res> SagaCompensationRegistry<E, Res> {
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
        }
    }

    pub fn register(&mut self, node_id: String, handler: SagaCompensationFn<E, Res>) {
        self.handlers.insert(node_id, handler);
    }

    pub fn get(&self, node_id: &str) -> Option<SagaCompensationFn<E, Res>> {
        self.handlers.get(node_id).cloned()
    }
}

/// Defines how the runtime should handle Saga compensations.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum SagaPolicy {
    /// No automated saga compensation.
    #[default]
    Disabled,
    /// Enable automated stack-based compensation (LIFO).
    Enabled,
}

/// Represents a successfully completed step that might need compensation later.
///
/// Captured automatically during Saga execution to enable "state-mapping"
/// for automated rollbacks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SagaTask {
    /// The ID of the node that completed successfully.
    pub node_id: String,
    /// The label of the node (for debugging/audit).
    pub node_label: String,
    /// JSON-serialized input that was passed to this node.
    /// This is used as the input for the compensation node.
    pub input_snapshot: Vec<u8>,
}

/// A stack of completed tasks that defines the compensation order (LIFO).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SagaStack {
    /// The sequence of completed tasks.
    pub tasks: Vec<SagaTask>,
}

impl SagaStack {
    /// Create a new empty Saga stack.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a successful step completion.
    pub fn push(&mut self, node_id: String, node_label: String, input_snapshot: Vec<u8>) {
        self.tasks.push(SagaTask {
            node_id,
            node_label,
            input_snapshot,
        });
    }

    /// Retrieve the last successful task for compensation.
    pub fn pop(&mut self) -> Option<SagaTask> {
        self.tasks.pop()
    }

    /// Check if there are tasks to compensate.
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// Clear the stack (e.g., after successful completion of all steps).
    pub fn clear(&mut self) {
        self.tasks.clear();
    }
}
