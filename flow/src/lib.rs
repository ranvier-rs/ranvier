//! Ranvier Flow - Typed State Tree Layer
//!
//! This crate defines the **structural** aspects of Ranvier:
//! - `FlowState`: The enum-based decision graph
//! - `Transition`: State transition contracts
//! - `Block`: Reusable sub-trees
//!
//! **IMPORTANT**: This layer is Pure Rust - no HTTP, no IO, no Async.

pub mod block;
pub mod state;
pub mod transition;

pub use block::{Block, BlockExecutor};
pub use state::FlowState;
pub use transition::{BranchTransition, Identity, Transition};
