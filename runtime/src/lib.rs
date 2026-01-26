//! Ranvier Runtime - Async Execution Layer
//!
//! This crate handles the **runtime** aspects of Ranvier:
//! - `Bus`: Type-safe resource injection
//! - `AsyncTransition`: Async state transitions
//! - `Executor`: Pipeline execution engine
//!
//! This layer depends on `ranvier-flow` for structural definitions.

pub mod async_transition;
pub mod bus;
pub mod executor;

pub use async_transition::AsyncTransition;
pub use bus::Bus;
pub use executor::Executor;
