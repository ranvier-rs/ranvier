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
    /// Execution paused at a node (debugger)
    NodePaused { node_id: String, timestamp: u64 },
    /// A branch decision was made
    Branchtaken { branch_id: String, timestamp: u64 },
    /// A faulted node is being retried (DLQ RetryThenDlq policy)
    NodeRetry {
        node_id: String,
        attempt: u32,
        max_attempts: u32,
        backoff_ms: u64,
        timestamp: u64,
    },
    /// All retry attempts exhausted; event sent to Dead Letter Queue
    DlqExhausted {
        node_id: String,
        total_attempts: u32,
        timestamp: u64,
    },
    /// A node execution exceeded the configured timeout
    NodeTimeout {
        node_id: String,
        timeout_ms: u64,
        timestamp: u64,
    },
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

    /// Sort events by timestamp while preserving insertion order for ties.
    ///
    /// Parallel execution uses deterministic phase/declaration ordering before
    /// insertion, so millisecond timestamp ties remain reproducible here.
    pub fn sort(&mut self) {
        self.events.sort_by_key(|e| match e {
            TimelineEvent::NodeEnter { timestamp, .. } => *timestamp,
            TimelineEvent::NodeExit { timestamp, .. } => *timestamp,
            TimelineEvent::NodePaused { timestamp, .. } => *timestamp,
            TimelineEvent::Branchtaken { timestamp, .. } => *timestamp,
            TimelineEvent::NodeRetry { timestamp, .. } => *timestamp,
            TimelineEvent::DlqExhausted { timestamp, .. } => *timestamp,
            TimelineEvent::NodeTimeout { timestamp, .. } => *timestamp,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{Timeline, TimelineEvent};

    #[test]
    fn sort_preserves_insertion_order_for_equal_timestamps() {
        let mut timeline = Timeline::new();
        timeline.push(TimelineEvent::NodePaused {
            node_id: "first".to_string(),
            timestamp: 42,
        });
        timeline.push(TimelineEvent::NodePaused {
            node_id: "second".to_string(),
            timestamp: 42,
        });

        timeline.sort();

        let ids = timeline
            .events
            .iter()
            .filter_map(|event| match event {
                TimelineEvent::NodePaused { node_id, .. } => Some(node_id.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["first", "second"]);
    }
}
