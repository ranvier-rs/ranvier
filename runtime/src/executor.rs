//! Executor - Async Pipeline Execution Engine
//!
//! The Executor runs state transitions through the typed state tree.

use crate::async_transition::AsyncTransition;
use crate::bus::Bus;

/// The runtime executor for state trees.
///
/// Executes async state transitions while maintaining
/// the typed state tree invariants.
pub struct Executor {
    bus: Bus,
}

impl Executor {
    /// Create a new executor with the given Bus
    pub fn new(bus: Bus) -> Self {
        Executor { bus }
    }

    /// Create an executor with an empty Bus
    pub fn empty() -> Self {
        Executor { bus: Bus::new() }
    }

    /// Get a reference to the Bus
    pub fn bus(&self) -> &Bus {
        &self.bus
    }

    /// Get a mutable reference to the Bus
    pub fn bus_mut(&mut self) -> &mut Bus {
        &mut self.bus
    }

    /// Execute a single async transition
    pub async fn step<T, From, To>(&self, from: From) -> Result<To, T::Error>
    where
        T: AsyncTransition<From, To, Context = Bus>,
        From: Send,
        To: Send,
    {
        T::transition(from, &self.bus).await
    }
}

impl Default for Executor {
    fn default() -> Self {
        Self::empty()
    }
}
