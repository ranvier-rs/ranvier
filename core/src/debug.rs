use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

/// Current execution state of the debugger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DebugState {
    /// Execution is proceeding normally.
    Running,
    /// Execution is currently paused at a node.
    Paused,
}

/// A controller used to manage breakpoints and execution flow for a running Axon.
///
/// This is typically stored in the `Bus` to allow the Axon runtime to check
/// for pause-points between node executions.
#[derive(Clone)]
pub struct DebugControl {
    inner: Arc<DebugControlInner>,
}

struct DebugControlInner {
    breakpoints: Mutex<HashSet<String>>,
    pause_next: Mutex<bool>,
    notify: Notify,
    state: Mutex<DebugState>,
}

impl DebugControl {
    /// Create a new DebugControl in the 'Running' state with no breakpoints.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(DebugControlInner {
                breakpoints: Mutex::new(HashSet::new()),
                pause_next: Mutex::new(false),
                notify: Notify::new(),
                state: Mutex::new(DebugState::Running),
            }),
        }
    }

    /// Add a breakpoint for a specific node ID.
    pub fn set_breakpoint(&self, node_id: String) {
        self.inner.breakpoints.lock().expect("debug mutex poisoned").insert(node_id);
    }

    /// Remove a breakpoint for a specific node ID.
    pub fn remove_breakpoint(&self, node_id: &str) {
        self.inner.breakpoints.lock().expect("debug mutex poisoned").remove(node_id);
    }

    /// Request execution to pause at the next available node.
    pub fn pause(&self) {
        *self.inner.pause_next.lock().expect("debug mutex poisoned") = true;
    }

    /// Resume execution from a paused state.
    pub fn resume(&self) {
        *self.inner.pause_next.lock().expect("debug mutex poisoned") = false;
        *self.inner.state.lock().expect("debug mutex poisoned") = DebugState::Running;
        self.inner.notify.notify_waiters();
    }

    /// Resume execution but pause again at the very next node.
    pub fn step(&self) {
        *self.inner.pause_next.lock().expect("debug mutex poisoned") = true;
        *self.inner.state.lock().expect("debug mutex poisoned") = DebugState::Running;
        self.inner.notify.notify_waiters();
    }

    /// Check if the current node should trigger a pause.
    ///
    /// This consumes the internal "pause_next" flag if it was set.
    pub fn should_pause(&self, node_id: &str) -> bool {
        let breakpoints = self.inner.breakpoints.lock().expect("debug mutex poisoned");
        let mut pause_next = self.inner.pause_next.lock().expect("debug mutex poisoned");
        let hit_breakpoint = breakpoints.contains(node_id);
        let pause_requested = *pause_next;

        if hit_breakpoint || pause_requested {
            *pause_next = false; // Consume the "step" or "pause" request
            true
        } else {
            false
        }
    }

    /// Explicitly transition to Paused state and wait for a resume signal.
    pub async fn wait(&self) {
        *self.inner.state.lock().expect("debug mutex poisoned") = DebugState::Paused;
        // Wait for resume() or step() to notify
        self.inner.notify.notified().await;
    }

    /// Check if the current node requires a pause and wait if so.
    ///
    /// Deprecated in favor of manual should_pause + wait for better event timing.
    pub async fn wait_if_needed(&self, node_id: &str) {
        if self.should_pause(node_id) {
            self.wait().await;
        }
    }

    /// Get the current debugger state.
    pub fn state(&self) -> DebugState {
        *self.inner.state.lock().expect("debug mutex poisoned")
    }

    /// List all currently set breakpoint node IDs.
    pub fn list_breakpoints(&self) -> Vec<String> {
        self.inner
            .breakpoints
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect()
    }
}

impl Default for DebugControl {
    fn default() -> Self {
        Self::new()
    }
}
