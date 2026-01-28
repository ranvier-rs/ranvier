use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type BranchId = String;
pub type NodeId = Uuid;

/// The explicit result of a transition in the Axon.
///
/// `Outcome` represents "Control Flow as Data".
/// Instead of implicit returns or exceptions, every state transition must return an `Outcome`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Outcome<T, E> {
    /// Proceed to the next node strictly (Linear flow)
    Next(T),

    /// Branch to a specific named path (Decision tree)
    Branch(BranchId, T),

    /// Jump to a specific Node ID (Loop / Goto)
    Jump(NodeId, T),

    /// Emit a side-effect event (Observability / Async Task)
    /// This does NOT change the state; the Axon continues with `T` (implicit next) or requires a subsequent `Next`.
    /// Actually, typically `Emit` might be combined or standalone.
    /// strictly: `Emit(Event, Box<Outcome<T,E>>)`?
    /// For simplicity in MVP, let's say Emit is a "Fire and Forget" that *also* carries the state to continue?
    /// Or Emit is a terminal leaf?
    /// Let's follow Discussion 162: `Emit(Event)` is a signal.
    /// We likely need a way to say "Emit this AND continue".
    /// Let's assume `Emit` returns the state `T` so the next node captures it.
    Emit(String, T),

    /// A structural fault (Error path)
    Fault(E),
}

impl<T, E> Outcome<T, E> {
    pub fn map<U, F: FnOnce(T) -> U>(self, op: F) -> Outcome<U, E> {
        match self {
            Outcome::Next(t) => Outcome::Next(op(t)),
            Outcome::Branch(id, t) => Outcome::Branch(id, op(t)),
            Outcome::Jump(id, t) => Outcome::Jump(id, op(t)),
            Outcome::Emit(evt, t) => Outcome::Emit(evt, op(t)),
            Outcome::Fault(e) => Outcome::Fault(e),
        }
    }
}
