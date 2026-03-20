//! # Closure-based Transition
//!
//! Provides `ClosureTransition`, a lightweight wrapper that implements
//! the `Transition` trait for synchronous closures. This eliminates
//! boilerplate for simple data-mapping or validation steps.
//!
//! ## Usage
//!
//! ```rust
//! use ranvier_runtime::prelude::*;
//! use ranvier_core::prelude::*;
//!
//! let pipeline = Axon::simple::<String>("pipeline")
//!     .then_fn("double", |input: (), bus: &mut Bus| {
//!         Outcome::next("result".to_string())
//!     });
//! ```

use async_trait::async_trait;
use ranvier_core::bus::Bus;
use ranvier_core::outcome::Outcome;
use ranvier_core::transition::{ResourceRequirement, Transition};
use std::fmt::Debug;
use std::marker::PhantomData;

/// A wrapper that adapts a synchronous closure into a `Transition`.
///
/// The closure receives `(From, &mut Bus)` and returns `Outcome<To, E>`.
/// Resources are not passed to the closure — closures that need typed
/// resources should use a full `#[transition]` struct instead.
///
/// The `Res` type parameter allows `ClosureTransition` to be used in
/// pipelines with any resource type — the resource is simply ignored
/// during execution.
pub struct ClosureTransition<F, Res = ()> {
    label: String,
    f: F,
    _phantom: PhantomData<Res>,
}

impl<F, Res> ClosureTransition<F, Res> {
    /// Create a new closure transition with the given label.
    pub fn new(label: impl Into<String>, f: F) -> Self {
        Self {
            label: label.into(),
            f,
            _phantom: PhantomData,
        }
    }
}

impl<F: Clone, Res> Clone for ClosureTransition<F, Res> {
    fn clone(&self) -> Self {
        Self {
            label: self.label.clone(),
            f: self.f.clone(),
            _phantom: PhantomData,
        }
    }
}

#[async_trait]
impl<F, From, To, E, Res> Transition<From, To> for ClosureTransition<F, Res>
where
    F: Fn(From, &mut Bus) -> Outcome<To, E> + Send + Sync + 'static,
    From: Send + 'static,
    To: Send + 'static,
    E: Send + Sync + Debug + 'static,
    Res: ResourceRequirement,
{
    type Error = E;
    type Resources = Res;

    fn label(&self) -> String {
        self.label.clone()
    }

    async fn run(
        &self,
        state: From,
        _resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<To, Self::Error> {
        (self.f)(state, bus)
    }
}
