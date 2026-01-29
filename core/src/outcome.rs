use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type BranchId = String;
pub type NodeId = Uuid;

/// The explicit result of a transition in the Axon.
///
/// `Outcome` represents "Control Flow as Data".
/// Instead of implicit returns or exceptions, every state transition must return an `Outcome`.
#[derive(Debug, Serialize, Deserialize)]
pub enum Outcome<T, E> {
    /// Proceed to the next node strictly (Linear flow)
    Next(T),

    /// Branch to a specific named path (Decision tree)
    /// The payload is type-erased to allow bubbling up through the Axon chain.
    #[serde(skip)]
    Branch(BranchId, Box<dyn std::any::Any + Send>),

    /// Jump to a specific Node ID (Loop / Goto)
    #[serde(skip)]
    Jump(NodeId, Box<dyn std::any::Any + Send>),

    /// Emit a side-effect event (Observability / Async Task)
    /// This acts as a signal carrier.
    #[serde(skip)]
    Emit(String, Box<dyn std::any::Any + Send>),

    /// A structural fault (Error path)
    Fault(E),
}

impl<T, E> Outcome<T, E> {
    pub fn map<U, F: FnOnce(T) -> U>(self, op: F) -> Outcome<U, E> {
        match self {
            Outcome::Next(t) => Outcome::Next(op(t)),
            Outcome::Branch(id, t) => Outcome::Branch(id, t),
            Outcome::Jump(id, t) => Outcome::Jump(id, t),
            Outcome::Emit(evt, t) => Outcome::Emit(evt, t),
            Outcome::Fault(e) => Outcome::Fault(e),
        }
    }
}
