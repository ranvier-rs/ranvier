//! # RanvierError: Serde-Compatible Error Wrapper
//!
//! A ready-made error type that satisfies the Axon serde bounds
//! (`Serialize + DeserializeOwned + Send + Sync + Debug`).
//!
//! Use this when you need typed error variants without defining a custom enum.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A serde-compatible error type for common Ranvier patterns.
///
/// This satisfies all Axon bounds (`Serialize + DeserializeOwned + Send + Sync + Debug`)
/// and provides typed variants for frequent error categories.
///
/// # When to Use
///
/// - **Prototyping**: When `String` is too unstructured but a custom enum is overkill
/// - **Examples**: As the standard error type in documentation examples
/// - **Production**: When error categories map cleanly to these variants
///
/// # Example
///
/// ```rust
/// use ranvier_core::error::RanvierError;
///
/// let err = RanvierError::not_found("User 42");
/// assert_eq!(err.to_string(), "not found: User 42");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RanvierError {
    /// Generic message error
    Message(String),
    /// Resource not found
    NotFound(String),
    /// Input validation failure
    Validation(String),
    /// Internal / unexpected error
    Internal(String),
}

impl RanvierError {
    /// Create a generic message error.
    pub fn message(msg: impl Into<String>) -> Self {
        Self::Message(msg.into())
    }

    /// Create a not-found error.
    pub fn not_found(what: impl Into<String>) -> Self {
        Self::NotFound(what.into())
    }

    /// Create a validation error.
    pub fn validation(msg: impl Into<String>) -> Self {
        Self::Validation(msg.into())
    }

    /// Create an internal error.
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::Internal(msg.into())
    }
}

impl fmt::Display for RanvierError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RanvierError::Message(s) => write!(f, "{s}"),
            RanvierError::NotFound(s) => write!(f, "not found: {s}"),
            RanvierError::Validation(s) => write!(f, "validation: {s}"),
            RanvierError::Internal(s) => write!(f, "internal: {s}"),
        }
    }
}

impl std::error::Error for RanvierError {}

impl From<String> for RanvierError {
    fn from(s: String) -> Self {
        RanvierError::Message(s)
    }
}

impl From<&str> for RanvierError {
    fn from(s: &str) -> Self {
        RanvierError::Message(s.to_string())
    }
}

/// Context automatically attached to the Bus when a Transition faults.
///
/// When a pipeline step returns `Outcome::Fault`, the Axon runtime stores
/// this struct in the Bus so downstream code can identify which step failed.
///
/// # Example
///
/// ```rust,ignore
/// let outcome = pipeline.run((), &(), &mut bus).await;
/// if let Outcome::Fault(_) = &outcome {
///     if let Some(ctx) = bus.read::<TransitionErrorContext>() {
///         eprintln!("Failed at step {} ({})", ctx.step_index, ctx.transition_name);
///     }
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionErrorContext {
    /// Name of the Axon pipeline.
    pub pipeline_name: String,
    /// Label of the Transition that produced the fault.
    pub transition_name: String,
    /// Zero-based step index within the pipeline.
    pub step_index: usize,
}

impl fmt::Display for TransitionErrorContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "pipeline '{}' step {} ({})",
            self.pipeline_name, self.step_index, self.transition_name,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display() {
        assert_eq!(RanvierError::message("oops").to_string(), "oops");
        assert_eq!(
            RanvierError::not_found("user 42").to_string(),
            "not found: user 42"
        );
    }

    #[test]
    fn test_serde_roundtrip() {
        let err = RanvierError::validation("bad email");
        let json = serde_json::to_string(&err).unwrap();
        let back: RanvierError = serde_json::from_str(&json).unwrap();
        assert_eq!(err, back);
    }

    #[test]
    fn test_from_string() {
        let err: RanvierError = "something went wrong".into();
        assert!(matches!(err, RanvierError::Message(_)));
    }
}
