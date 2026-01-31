use crate::timeline::{Timeline, TimelineEvent};

/// ReplayEngine reconstructs the execution state from a Timeline.
/// It creates a "virtual" cursor that moves through the circuit based on recorded events.
pub struct ReplayEngine {
    timeline: Timeline,
    cursor: usize,
}

#[derive(Debug, Clone)]
pub struct ReplayFrame {
    pub current_node_id: Option<String>,
    pub event: TimelineEvent,
}

impl ReplayEngine {
    pub fn new(timeline: Timeline) -> Self {
        Self {
            timeline,
            cursor: 0,
        }
    }

    /// Advance the replay by one step.
    /// Returns the current frame or None if finished.
    pub fn next_step(&mut self) -> Option<ReplayFrame> {
        if self.cursor >= self.timeline.events.len() {
            return None;
        }

        let event = self.timeline.events[self.cursor].clone();
        self.cursor += 1;

        let current_node_id = match &event {
            TimelineEvent::NodeEnter { node_id, .. } => Some(node_id.clone()),
            TimelineEvent::NodeExit { node_id, .. } => Some(node_id.clone()),
            TimelineEvent::Branchtaken { .. } => None, // Branches happen "between" nodes conceptually or part of outcome
        };

        Some(ReplayFrame {
            current_node_id,
            event,
        })
    }

    /// Reset replay to start
    pub fn reset(&mut self) {
        self.cursor = 0;
    }
}
