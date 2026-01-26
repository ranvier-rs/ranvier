//! Transition - State Transition Contracts
//!
//! Transitions define how one state moves to another.
//! This is the "Pipe" in Ranvier's philosophy - a contract for state change.
//!
//! # Philosophy
//! > Pipe = State Transition Contract
//!
//! Each transition is explicit, type-safe, and compile-time verified.

/// The core trait for state transitions.
///
/// A `Transition<From, To>` defines how to move from state `From` to state `To`.
///
/// # Type Parameters
/// - `From`: The source state type
/// - `To`: The target state type
///
/// # Example
/// ```rust,ignore
/// struct AuthTransition;
///
/// impl Transition<RawRequest, AuthResult> for AuthTransition {
///     type Error = AuthError;
///     type Context = AppContext;
///
///     fn transition(from: RawRequest, ctx: &Self::Context) -> Result<AuthResult, Self::Error> {
///         // Validate auth header, return Authenticated or Unauthorized
///     }
/// }
/// ```
pub trait Transition<From, To> {
    /// Error type for this transition
    type Error;

    /// Context type (Bus) for resource access
    type Context;

    /// Perform the state transition
    ///
    /// This is a synchronous operation. For async transitions,
    /// use the runtime layer's `AsyncTransition`.
    fn transition(from: From, ctx: &Self::Context) -> Result<To, Self::Error>;
}

/// A transition that can branch to multiple target states.
///
/// This is the "decision tree" aspect - based on the input,
/// the flow can branch to different states.
///
/// # Example
/// ```rust,ignore
/// enum AuthResult {
///     Authenticated(User),
///     Unauthorized,
///     RequiresMFA(MfaChallenge),
/// }
///
/// impl BranchTransition<RawRequest> for AuthBranch {
///     type Output = AuthResult;
///     type Error = AuthError;
///     type Context = AppContext;
///
///     fn branch(from: RawRequest, ctx: &Self::Context) -> Result<AuthResult, Self::Error> {
///         match validate_token(&from, ctx) {
///             Ok(user) => Ok(AuthResult::Authenticated(user)),
///             Err(TokenExpired) => Ok(AuthResult::RequiresMFA(challenge)),
///             Err(e) => Err(AuthError::from(e)),
///         }
///     }
/// }
/// ```
pub trait BranchTransition<From> {
    /// The output enum that represents all possible branches
    type Output;

    /// Error type
    type Error;

    /// Context type for resource access
    type Context;

    /// Perform the branching transition
    fn branch(from: From, ctx: &Self::Context) -> Result<Self::Output, Self::Error>;
}

/// Identity transition - pass through unchanged
///
/// Use `Identity<YourContext>` to create an identity transition
/// for a specific context type.
pub struct Identity<Ctx>(std::marker::PhantomData<Ctx>);

impl<Ctx> Identity<Ctx> {
    /// Create a new identity transition
    pub fn new() -> Self {
        Identity(std::marker::PhantomData)
    }
}

impl<Ctx> Default for Identity<Ctx> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, Ctx> Transition<T, T> for Identity<Ctx> {
    type Error = std::convert::Infallible;
    type Context = Ctx;

    fn transition(from: T, _ctx: &Self::Context) -> Result<T, Self::Error> {
        Ok(from)
    }
}
