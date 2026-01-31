//! # Outcome: Explicit Control Flow as Data
//!
//! `Outcome` represents "Control Flow as Data".
//! Instead of implicit returns or exceptions, every state transition must return an `Outcome`.
//!
//! ## Design Philosophy
//!
//! * Errors are **alternative paths**, not exceptions
//! * Branching is **explicit**, not implicit
//! * Control flow is **visible** in the Schematic

use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Import anyhow for the into_result conversion
use __private::anyhow;

// Private module to hide anyhow from the public API
mod __private {
    pub use anyhow;
}

pub type BranchId = String;
pub type NodeId = Uuid;

/// The explicit result of a transition in the Axon.
///
/// Every transition returns an `Outcome` that determines:
/// * What happens next (Next node, Branch, Jump)
/// * What data is passed to the next step
/// * Whether the flow encountered an error
///
/// ## Variants
///
/// * **Next(T)** - Proceed to the next node linearly with data T
/// * **Branch(id, payload)** - Branch to a named path with serialized payload
/// * **Jump(id, payload)** - Jump to a specific Node ID (loop/goto)
/// * **Emit(event_type, payload)** - Emit a side-effect event
/// * **Fault(E)** - An error occurred (error path)
///
/// ## Serialization
///
/// All variants are serializable to support Schematic JSON export.
/// Payloads use `serde_json::Value` for type-erased but serializable data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Outcome<T, E> {
    /// Proceed to the next node strictly (Linear flow)
    Next(T),

    /// Branch to a specific named path (Decision tree).
    /// The payload is a serializable JSON value for cross-boundary transmission.
    Branch(BranchId, Option<serde_json::Value>),

    /// Jump to a specific Node ID (Loop / Goto).
    /// The payload is a serializable JSON value.
    Jump(NodeId, Option<serde_json::Value>),

    /// Emit a side-effect event (Observability / Async Task).
    /// This acts as a signal carrier without breaking the flow.
    Emit(String, Option<serde_json::Value>),

    /// A structural fault (Error path)
    Fault(E),
}

impl<T, E> Outcome<T, E> {
    /// Map the success value through a function.
    ///
    /// Preserves control flow variants (Branch, Jump, Emit, Fault) unchanged.
    pub fn map<U, F: FnOnce(T) -> U>(self, op: F) -> Outcome<U, E> {
        match self {
            Outcome::Next(t) => Outcome::Next(op(t)),
            Outcome::Branch(id, payload) => Outcome::Branch(id, payload),
            Outcome::Jump(id, payload) => Outcome::Jump(id, payload),
            Outcome::Emit(evt, payload) => Outcome::Emit(evt, payload),
            Outcome::Fault(e) => Outcome::Fault(e),
        }
    }

    /// Map the error value through a function.
    ///
    /// Preserves control flow variants unchanged.
    pub fn map_err<F, G: FnOnce(E) -> F>(self, op: G) -> Outcome<T, F> {
        match self {
            Outcome::Next(t) => Outcome::Next(t),
            Outcome::Branch(id, payload) => Outcome::Branch(id, payload),
            Outcome::Jump(id, payload) => Outcome::Jump(id, payload),
            Outcome::Emit(evt, payload) => Outcome::Emit(evt, payload),
            Outcome::Fault(e) => Outcome::Fault(op(e)),
        }
    }

    /// Convert to a Result, treating all non-Next variants as errors.
    ///
    /// This method is only available when `E: From<anyhow::Error>`.
    pub fn into_result(self) -> Result<T, E>
    where
        E: std::convert::From<anyhow::Error>,
    {
        match self {
            Outcome::Next(t) => Ok(t),
            Outcome::Fault(e) => Err(e),
            // Branch, Jump, Emit are treated as early termination
            Outcome::Branch(_, _) => Err(anyhow::anyhow!("Early termination: Branch").into()),
            Outcome::Jump(_, _) => Err(anyhow::anyhow!("Early termination: Jump").into()),
            Outcome::Emit(_, _) => Err(anyhow::anyhow!("Early termination: Emit").into()),
        }
    }

    /// Check if this outcome represents a fault/error.
    pub fn is_fault(&self) -> bool {
        matches!(self, Outcome::Fault(_))
    }

    /// Check if this outcome represents a branch.
    pub fn is_branch(&self) -> bool {
        matches!(self, Outcome::Branch(_, _))
    }

    /// Check if this outcome represents a jump.
    pub fn is_jump(&self) -> bool {
        matches!(self, Outcome::Jump(_, _))
    }

    /// Check if this outcome is a simple Next (linear progression).
    pub fn is_next(&self) -> bool {
        matches!(self, Outcome::Next(_))
    }

    /// Check if this outcome represents an emit.
    pub fn is_emit(&self) -> bool {
        matches!(self, Outcome::Emit(_, _))
    }
}

impl<T: Serialize, E: Serialize> Outcome<T, E> {
    /// Convert the payload to a JSON value for Next variant.
    pub fn to_json_value(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}

/// Helper constructors for Outcome
impl<T, E> Outcome<T, E> {
    /// Create a Next outcome
    pub fn next(value: T) -> Self {
        Self::Next(value)
    }

    /// Create a Branch outcome with optional JSON payload
    pub fn branch(id: impl Into<String>, payload: Option<serde_json::Value>) -> Self {
        Self::Branch(id.into(), payload)
    }

    /// Create a Jump outcome with optional JSON payload
    pub fn jump(id: Uuid, payload: Option<serde_json::Value>) -> Self {
        Self::Jump(id, payload)
    }

    /// Create an Emit outcome with optional JSON payload
    pub fn emit(event_type: impl Into<String>, payload: Option<serde_json::Value>) -> Self {
        Self::Emit(event_type.into(), payload)
    }

    /// Create a Fault outcome
    pub fn fault(error: E) -> Self {
        Self::Fault(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_outcome_next() {
        let outcome: Outcome<(), String> = Outcome::next(());
        assert!(outcome.is_next());
        assert!(!outcome.is_fault());
    }

    #[test]
    fn test_outcome_map() {
        let outcome: Outcome<i32, String> = Outcome::next(42);
        let mapped = outcome.map(|x| x * 2);
        assert!(matches!(mapped, Outcome::Next(84)));
    }

    #[test]
    fn test_outcome_branch() {
        let outcome: Outcome<(), String> = Outcome::branch("auth_failed", None);
        assert!(outcome.is_branch());
    }

    #[test]
    fn test_outcome_serialization() {
        let outcome: Outcome<i32, String> = Outcome::next(42);
        let json = serde_json::to_string(&outcome).unwrap();
        assert!(json.contains("Next"));
    }
}
