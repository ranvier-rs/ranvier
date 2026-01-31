use serde::{Deserialize, Serialize};

/// Represents a discrete event in the execution timeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TimelineEvent {
    /// Execution started at a node
    NodeEnter {
        node_id: String,
        node_label: String,
        timestamp: u64,
    },
    /// Execution finished at a node
    NodeExit {
        node_id: String,
        outcome_type: String, // "Next", "Branch", "Error"
        duration_ms: u64,
        timestamp: u64,
    },
    /// A branch decision was made
    Branchtaken { branch_id: String, timestamp: u64 },
}

/// A sequential record of an execution session.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Timeline {
    pub events: Vec<TimelineEvent>,
}

impl Timeline {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, event: TimelineEvent) {
        self.events.push(event);
    }

    /// Sort events by timestamp
    pub fn sort(&mut self) {
        self.events.sort_by_key(|e| match e {
            TimelineEvent::NodeEnter { timestamp, .. } => *timestamp,
            TimelineEvent::NodeExit { timestamp, .. } => *timestamp,
            TimelineEvent::Branchtaken { timestamp, .. } => *timestamp,
        });
    }
}
