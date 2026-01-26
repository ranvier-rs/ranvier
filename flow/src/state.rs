//! FlowState - The Typed State Tree
//!
//! All execution flows through enum-defined states that move through
//! match-based decision trees. This is the core execution model of Ranvier.
//!
//! # Example
//! ```rust
//! use ranvier_flow::FlowState;
//!
//! enum AppState {
//!     RawRequest(HttpRequest),
//!     Authenticated(AuthContext),
//!     Processed(DomainResult),
//!     Response(HttpResponse),
//! }
//! ```

/// The core state wrapper for typed state trees.
///
/// `S` is the state enum (user-defined).
/// `E` is the error type.
#[derive(Debug, Clone)]
pub enum FlowState<S, E> {
    /// Active state holding the current value
    Active(S),
    /// Terminal state - flow completed successfully
    Terminal(S),
    /// Error state - flow terminated with error
    Error(E),
}

impl<S, E> FlowState<S, E> {
    /// Create a new active state
    pub fn active(state: S) -> Self {
        FlowState::Active(state)
    }

    /// Create a terminal state
    pub fn terminal(state: S) -> Self {
        FlowState::Terminal(state)
    }

    /// Create an error state
    pub fn error(err: E) -> Self {
        FlowState::Error(err)
    }

    /// Check if the flow is still active
    pub fn is_active(&self) -> bool {
        matches!(self, FlowState::Active(_))
    }

    /// Check if the flow has terminated
    pub fn is_terminal(&self) -> bool {
        matches!(self, FlowState::Terminal(_))
    }

    /// Check if the flow has errored
    pub fn is_error(&self) -> bool {
        matches!(self, FlowState::Error(_))
    }

    /// Extract the inner state if active
    pub fn into_active(self) -> Option<S> {
        match self {
            FlowState::Active(s) => Some(s),
            _ => None,
        }
    }

    /// Extract the inner state if terminal
    pub fn into_terminal(self) -> Option<S> {
        match self {
            FlowState::Terminal(s) => Some(s),
            _ => None,
        }
    }

    /// Map the state type
    pub fn map<F, S2>(self, f: F) -> FlowState<S2, E>
    where
        F: FnOnce(S) -> S2,
    {
        match self {
            FlowState::Active(s) => FlowState::Active(f(s)),
            FlowState::Terminal(s) => FlowState::Terminal(f(s)),
            FlowState::Error(e) => FlowState::Error(e),
        }
    }

    /// Map the error type
    pub fn map_err<F, E2>(self, f: F) -> FlowState<S, E2>
    where
        F: FnOnce(E) -> E2,
    {
        match self {
            FlowState::Active(s) => FlowState::Active(s),
            FlowState::Terminal(s) => FlowState::Terminal(s),
            FlowState::Error(e) => FlowState::Error(f(e)),
        }
    }
}

/// A marker trait for state enums that can participate in a flow.
///
/// Implement this for your state enum to enable compile-time
/// verification of state transitions.
pub trait State: Sized {}

/// Result type for state transitions
pub type TransitionResult<S, E> = Result<FlowState<S, E>, E>;
