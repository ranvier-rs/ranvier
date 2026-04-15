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

    /// Map the fault (error) value through a function.
    ///
    /// Alias for [`map_err`](Outcome::map_err) using Ranvier's `Fault` naming convention.
    /// Preserves all non-Fault variants unchanged.
    pub fn map_fault<F, G: FnOnce(E) -> F>(self, op: G) -> Outcome<T, F> {
        self.map_err(op)
    }

    /// Chain a computation that may produce another Outcome.
    ///
    /// If `self` is `Next(t)`, applies `f(t)` and returns the result.
    /// All other variants (Branch, Jump, Emit, Fault) are passed through unchanged.
    pub fn and_then<U, F: FnOnce(T) -> Outcome<U, E>>(self, op: F) -> Outcome<U, E> {
        match self {
            Outcome::Next(t) => op(t),
            Outcome::Branch(id, payload) => Outcome::Branch(id, payload),
            Outcome::Jump(id, payload) => Outcome::Jump(id, payload),
            Outcome::Emit(evt, payload) => Outcome::Emit(evt, payload),
            Outcome::Fault(e) => Outcome::Fault(e),
        }
    }

    /// Extract the `Next` value, or return `default` for any other variant.
    pub fn unwrap_or(self, default: T) -> T {
        match self {
            Outcome::Next(t) => t,
            _ => default,
        }
    }

    /// Extract the `Next` value, or compute a default from a closure.
    pub fn unwrap_or_else<F: FnOnce() -> T>(self, f: F) -> T {
        match self {
            Outcome::Next(t) => t,
            _ => f(),
        }
    }
}

/// Convert a `Result<T, E>` into an `Outcome<T, String>`.
///
/// - `Ok(v)` becomes `Outcome::Next(v)`
/// - `Err(e)` becomes `Outcome::Fault(e.to_string())`
impl<T> Outcome<T, String> {
    pub fn from_result<E2: std::fmt::Display>(result: Result<T, E2>) -> Self {
        match result {
            Ok(v) => Outcome::Next(v),
            Err(e) => Outcome::Fault(e.to_string()),
        }
    }

    /// Convert a `Result<T, E>` into an `Outcome<T, String>` with context prefix.
    ///
    /// - `Ok(v)` becomes `Outcome::Next(v)`
    /// - `Err(e)` becomes `Outcome::Fault("{context}: {e}")`
    pub fn from_result_ctx<E2: std::fmt::Display>(result: Result<T, E2>, context: &str) -> Self {
        match result {
            Ok(v) => Outcome::Next(v),
            Err(e) => Outcome::Fault(format!("{context}: {e}")),
        }
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

    #[test]
    fn test_outcome_map_preserves_branch() {
        let outcome: Outcome<i32, String> = Outcome::branch("error_path", None);
        let mapped = outcome.map(|x| x * 2);
        assert!(mapped.is_branch());
        assert!(!mapped.is_next());
    }

    #[test]
    fn test_outcome_map_preserves_fault() {
        let outcome: Outcome<i32, String> = Outcome::fault("error".to_string());
        let mapped = outcome.map(|x| x * 2);
        assert!(mapped.is_fault());
    }

    #[test]
    fn test_outcome_map_preserves_jump() {
        let node_id = Uuid::new_v4();
        let outcome: Outcome<i32, String> = Outcome::jump(node_id, None);
        let mapped = outcome.map(|x| x * 2);
        assert!(mapped.is_jump());
    }

    #[test]
    fn test_outcome_map_preserves_emit() {
        let outcome: Outcome<i32, String> = Outcome::emit("user_created", None);
        let mapped = outcome.map(|x| x * 2);
        assert!(mapped.is_emit());
    }

    #[test]
    fn test_outcome_map_err_transforms_fault() {
        let outcome: Outcome<i32, String> = Outcome::fault("original_error".to_string());
        let mapped = outcome.map_err(|e| format!("wrapped: {}", e));
        match mapped {
            Outcome::Fault(e) => assert_eq!(e, "wrapped: original_error"),
            _ => panic!("Expected Fault variant"),
        }
    }

    #[test]
    fn test_outcome_map_err_preserves_next() {
        let outcome: Outcome<i32, String> = Outcome::next(42);
        let mapped = outcome.map_err(|e| format!("wrapped: {}", e));
        assert!(matches!(mapped, Outcome::Next(42)));
    }

    #[test]
    fn test_outcome_map_err_preserves_branch() {
        let outcome: Outcome<i32, String> = Outcome::branch("auth_failed", None);
        let mapped = outcome.map_err(|e| format!("wrapped: {}", e));
        assert!(mapped.is_branch());
    }

    #[test]
    fn test_outcome_is_next_check() {
        let outcome: Outcome<i32, String> = Outcome::next(42);
        assert!(outcome.is_next());
        assert!(!outcome.is_fault());
        assert!(!outcome.is_branch());
        assert!(!outcome.is_jump());
        assert!(!outcome.is_emit());
    }

    #[test]
    fn test_outcome_is_fault_check() {
        let outcome: Outcome<i32, String> = Outcome::fault("error".to_string());
        assert!(outcome.is_fault());
        assert!(!outcome.is_next());
        assert!(!outcome.is_branch());
        assert!(!outcome.is_jump());
        assert!(!outcome.is_emit());
    }

    #[test]
    fn test_outcome_is_branch_check() {
        let outcome: Outcome<i32, String> = Outcome::branch("path", None);
        assert!(outcome.is_branch());
        assert!(!outcome.is_next());
        assert!(!outcome.is_fault());
        assert!(!outcome.is_jump());
        assert!(!outcome.is_emit());
    }

    #[test]
    fn test_outcome_is_jump_check() {
        let node_id = Uuid::new_v4();
        let outcome: Outcome<i32, String> = Outcome::jump(node_id, None);
        assert!(outcome.is_jump());
        assert!(!outcome.is_next());
        assert!(!outcome.is_fault());
        assert!(!outcome.is_branch());
        assert!(!outcome.is_emit());
    }

    #[test]
    fn test_outcome_is_emit_check() {
        let outcome: Outcome<i32, String> = Outcome::emit("event", None);
        assert!(outcome.is_emit());
        assert!(!outcome.is_next());
        assert!(!outcome.is_fault());
        assert!(!outcome.is_branch());
        assert!(!outcome.is_jump());
    }

    #[test]
    fn test_outcome_serialization_roundtrip_next() {
        let original: Outcome<i32, String> = Outcome::next(42);
        let json = serde_json::to_string(&original).unwrap();
        let deserialized: Outcome<i32, String> = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, Outcome::Next(42)));
    }

    #[test]
    fn test_outcome_serialization_roundtrip_fault() {
        let original: Outcome<i32, String> = Outcome::fault("error_message".to_string());
        let json = serde_json::to_string(&original).unwrap();
        let deserialized: Outcome<i32, String> = serde_json::from_str(&json).unwrap();
        match deserialized {
            Outcome::Fault(e) => assert_eq!(e, "error_message"),
            _ => panic!("Expected Fault variant"),
        }
    }

    #[test]
    fn test_outcome_serialization_roundtrip_branch() {
        let payload = serde_json::json!({"reason": "unauthorized"});
        let original: Outcome<i32, String> = Outcome::branch("auth_failed", Some(payload.clone()));
        let json = serde_json::to_string(&original).unwrap();
        let deserialized: Outcome<i32, String> = serde_json::from_str(&json).unwrap();
        match deserialized {
            Outcome::Branch(id, p) => {
                assert_eq!(id, "auth_failed");
                assert_eq!(p, Some(payload));
            }
            _ => panic!("Expected Branch variant"),
        }
    }

    #[test]
    fn test_outcome_serialization_roundtrip_jump() {
        let node_id = Uuid::new_v4();
        let payload = serde_json::json!({"state": "retry"});
        let original: Outcome<i32, String> = Outcome::jump(node_id, Some(payload.clone()));
        let json = serde_json::to_string(&original).unwrap();
        let deserialized: Outcome<i32, String> = serde_json::from_str(&json).unwrap();
        match deserialized {
            Outcome::Jump(id, p) => {
                assert_eq!(id, node_id);
                assert_eq!(p, Some(payload));
            }
            _ => panic!("Expected Jump variant"),
        }
    }

    #[test]
    fn test_outcome_serialization_roundtrip_emit() {
        let payload = serde_json::json!({"user_id": 123});
        let original: Outcome<i32, String> = Outcome::emit("user_created", Some(payload.clone()));
        let json = serde_json::to_string(&original).unwrap();
        let deserialized: Outcome<i32, String> = serde_json::from_str(&json).unwrap();
        match deserialized {
            Outcome::Emit(evt, p) => {
                assert_eq!(evt, "user_created");
                assert_eq!(p, Some(payload));
            }
            _ => panic!("Expected Emit variant"),
        }
    }

    #[test]
    fn test_outcome_into_result_next_success() {
        let outcome: Outcome<i32, anyhow::Error> = Outcome::next(42);
        let result = outcome.into_result();
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
    }

    #[test]
    fn test_outcome_into_result_fault_error() {
        let outcome: Outcome<i32, anyhow::Error> = Outcome::fault(anyhow::anyhow!("test error"));
        let result = outcome.into_result();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), "test error");
    }

    #[test]
    fn test_outcome_into_result_branch_error() {
        let outcome: Outcome<i32, anyhow::Error> = Outcome::branch("path", None);
        let result = outcome.into_result();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Early termination: Branch")
        );
    }

    #[test]
    fn test_outcome_into_result_jump_error() {
        let node_id = Uuid::new_v4();
        let outcome: Outcome<i32, anyhow::Error> = Outcome::jump(node_id, None);
        let result = outcome.into_result();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Early termination: Jump")
        );
    }

    #[test]
    fn test_outcome_into_result_emit_error() {
        let outcome: Outcome<i32, anyhow::Error> = Outcome::emit("event", None);
        let result = outcome.into_result();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Early termination: Emit")
        );
    }

    #[test]
    fn test_outcome_emit_creates_event_emission() {
        let outcome: Outcome<i32, String> = Outcome::emit("user_registered", None);
        assert!(outcome.is_emit());
        match outcome {
            Outcome::Emit(event_type, payload) => {
                assert_eq!(event_type, "user_registered");
                assert_eq!(payload, None);
            }
            _ => panic!("Expected Emit variant"),
        }
    }

    #[test]
    fn test_outcome_emit_with_payload() {
        let payload = serde_json::json!({"user_id": 456, "email": "test@example.com"});
        let outcome: Outcome<i32, String> = Outcome::emit("user_registered", Some(payload.clone()));
        assert!(outcome.is_emit());
        match outcome {
            Outcome::Emit(event_type, p) => {
                assert_eq!(event_type, "user_registered");
                assert_eq!(p, Some(payload));
            }
            _ => panic!("Expected Emit variant"),
        }
    }

    // ── M342: map_fault ───────────────────────────────────────────

    #[test]
    fn test_map_fault_transforms_fault() {
        let outcome: Outcome<i32, String> = Outcome::fault("original".into());
        let mapped = outcome.map_fault(|e| format!("wrapped: {e}"));
        match mapped {
            Outcome::Fault(e) => assert_eq!(e, "wrapped: original"),
            _ => panic!("Expected Fault"),
        }
    }

    #[test]
    fn test_map_fault_preserves_next() {
        let outcome: Outcome<i32, String> = Outcome::next(42);
        let mapped = outcome.map_fault(|e| format!("wrapped: {e}"));
        assert!(matches!(mapped, Outcome::Next(42)));
    }

    // ── M342: and_then ────────────────────────────────────────────

    #[test]
    fn test_and_then_applies_on_next() {
        let outcome: Outcome<i32, String> = Outcome::next(21);
        let chained = outcome.and_then(|v| Outcome::Next(v * 2));
        assert!(matches!(chained, Outcome::Next(42)));
    }

    #[test]
    fn test_and_then_propagates_fault() {
        let outcome: Outcome<i32, String> = Outcome::fault("err".into());
        let chained = outcome.and_then(|v| Outcome::Next(v * 2));
        assert!(chained.is_fault());
    }

    #[test]
    fn test_and_then_propagates_branch() {
        let outcome: Outcome<i32, String> = Outcome::branch("path_a", None);
        let chained = outcome.and_then(|v| Outcome::Next(v * 2));
        assert!(chained.is_branch());
    }

    #[test]
    fn test_and_then_can_produce_fault() {
        let outcome: Outcome<i32, String> = Outcome::next(0);
        let chained = outcome.and_then(|v| {
            if v == 0 {
                Outcome::Fault("division by zero".into())
            } else {
                Outcome::Next(100 / v)
            }
        });
        assert!(chained.is_fault());
    }

    #[test]
    fn test_and_then_chain() {
        let result: Outcome<i32, String> = Outcome::next(10)
            .and_then(|v| Outcome::Next(v + 5))
            .and_then(|v| Outcome::Next(v * 2));
        assert!(matches!(result, Outcome::Next(30)));
    }

    // ── M342: unwrap_or / unwrap_or_else ──────────────────────────

    #[test]
    fn test_unwrap_or_returns_value_on_next() {
        let outcome: Outcome<i32, String> = Outcome::next(42);
        assert_eq!(outcome.unwrap_or(0), 42);
    }

    #[test]
    fn test_unwrap_or_returns_default_on_fault() {
        let outcome: Outcome<i32, String> = Outcome::fault("err".into());
        assert_eq!(outcome.unwrap_or(0), 0);
    }

    #[test]
    fn test_unwrap_or_returns_default_on_branch() {
        let outcome: Outcome<i32, String> = Outcome::branch("path", None);
        assert_eq!(outcome.unwrap_or(-1), -1);
    }

    #[test]
    fn test_unwrap_or_else_on_next() {
        let outcome: Outcome<i32, String> = Outcome::next(42);
        assert_eq!(outcome.unwrap_or_else(|| 0), 42);
    }

    #[test]
    fn test_unwrap_or_else_on_fault() {
        let outcome: Outcome<i32, String> = Outcome::fault("err".into());
        assert_eq!(outcome.unwrap_or_else(|| 99), 99);
    }

    // ── M342: from_result / from_result_ctx ───────────────────────

    #[test]
    fn test_from_result_ok() {
        let result: Result<i32, std::num::ParseIntError> = "42".parse();
        let outcome = Outcome::from_result(result);
        assert!(matches!(outcome, Outcome::Next(42)));
    }

    #[test]
    fn test_from_result_err() {
        let result: Result<i32, std::num::ParseIntError> = "abc".parse();
        let outcome = Outcome::from_result(result);
        assert!(outcome.is_fault());
        match outcome {
            Outcome::Fault(e) => assert!(e.contains("invalid digit")),
            _ => panic!("Expected Fault"),
        }
    }

    #[test]
    fn test_from_result_ctx_ok() {
        let result: Result<i32, std::num::ParseIntError> = "42".parse();
        let outcome = Outcome::from_result_ctx(result, "parse int");
        assert!(matches!(outcome, Outcome::Next(42)));
    }

    #[test]
    fn test_from_result_ctx_err() {
        let result: Result<i32, std::num::ParseIntError> = "abc".parse();
        let outcome = Outcome::from_result_ctx(result, "parse int");
        match outcome {
            Outcome::Fault(e) => {
                assert!(e.starts_with("parse int: "));
                assert!(e.contains("invalid digit"));
            }
            _ => panic!("Expected Fault"),
        }
    }

    // ── M342: try_outcome! macro ──────────────────────────────────

    fn helper_try_outcome_ok() -> Outcome<i32, String> {
        let val = crate::try_outcome!("42".parse::<i32>());
        Outcome::Next(val * 2)
    }

    fn helper_try_outcome_err() -> Outcome<i32, String> {
        let val = crate::try_outcome!("abc".parse::<i32>());
        Outcome::Next(val * 2)
    }

    fn helper_try_outcome_ctx_err() -> Outcome<i32, String> {
        let val = crate::try_outcome!("abc".parse::<i32>(), "parse failed");
        Outcome::Next(val * 2)
    }

    fn helper_try_outcome_ctx_ok() -> Outcome<i32, String> {
        let val = crate::try_outcome!("7".parse::<i32>(), "parse failed");
        Outcome::Next(val + 3)
    }

    #[test]
    fn test_try_outcome_success() {
        let outcome = helper_try_outcome_ok();
        assert!(matches!(outcome, Outcome::Next(84)));
    }

    #[test]
    fn test_try_outcome_failure() {
        let outcome = helper_try_outcome_err();
        assert!(outcome.is_fault());
        match outcome {
            Outcome::Fault(e) => assert!(e.contains("invalid digit")),
            _ => panic!("Expected Fault"),
        }
    }

    #[test]
    fn test_try_outcome_with_context_success() {
        let outcome = helper_try_outcome_ctx_ok();
        assert!(matches!(outcome, Outcome::Next(10)));
    }

    #[test]
    fn test_try_outcome_with_context_failure() {
        let outcome = helper_try_outcome_ctx_err();
        match outcome {
            Outcome::Fault(e) => {
                assert!(e.starts_with("parse failed: "));
                assert!(e.contains("invalid digit"));
            }
            _ => panic!("Expected Fault"),
        }
    }

    // ── M342: combinator chains ───────────────────────────────────

    #[test]
    fn test_combinator_chain_map_and_map_fault() {
        let outcome: Outcome<i32, String> = Outcome::next(10);
        let result = outcome.map(|v| v + 5).map_fault(|e| format!("ERR: {e}"));
        assert!(matches!(result, Outcome::Next(15)));
    }

    #[test]
    fn test_combinator_chain_fault_path() {
        let outcome: Outcome<i32, i32> = Outcome::fault(404);
        let result = outcome
            .map(|v| v + 5)
            .map_fault(|code| format!("HTTP {code}"));
        match result {
            Outcome::Fault(msg) => assert_eq!(msg, "HTTP 404"),
            _ => panic!("Expected Fault"),
        }
    }
}
