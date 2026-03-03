use ranvier_core::timeline::{Timeline, TimelineEvent};

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
            TimelineEvent::NodePaused { node_id, .. } => Some(node_id.clone()),
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

    /// Fast-forwards the replay cursor to the end, returning the final known frame.
    /// This is a O(1) operation since it just jumps to the end of the timeline array.
    pub fn fast_forward_to_end(&mut self) -> Option<ReplayFrame> {
        if self.timeline.events.is_empty() {
            return None;
        }
        self.cursor = self.timeline.events.len() - 1;
        self.next_step()
    }

    /// Fast-forwards the replay cursor to the last active (entered but not exited) node.
    /// This is useful for active intervention where we want to resume execution at the exact stalled point.
    pub fn fast_forward_to_active(&mut self) -> Option<ReplayFrame> {
        let mut exited_nodes = std::collections::HashSet::new();
        let mut active_index = None;
        
        for i in (0..self.timeline.events.len()).rev() {
            match &self.timeline.events[i] {
                TimelineEvent::NodeExit { node_id, .. } => {
                    exited_nodes.insert(node_id.clone());
                }
                TimelineEvent::NodeEnter { node_id, .. } | TimelineEvent::NodePaused { node_id, .. } => {
                    if !exited_nodes.contains(node_id) {
                        active_index = Some(i);
                        break;
                    } else {
                        exited_nodes.remove(node_id);
                    }
                }
                _ => {}
            }
        }

        if let Some(index) = active_index {
            self.cursor = index;
            self.next_step()
        } else {
            self.fast_forward_to_end()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ranvier_core::timeline::{Timeline, TimelineEvent};

    fn test_event(node_id: &str, enter: bool) -> TimelineEvent {
        if enter {
            TimelineEvent::NodeEnter {
                node_id: node_id.to_string(),
                node_label: node_id.to_string(),
                timestamp: 0,
            }
        } else {
            TimelineEvent::NodeExit {
                node_id: node_id.to_string(),
                outcome_type: "Next".to_string(),
                duration_ms: 0,
                timestamp: 0,
            }
        }
    }

    #[test]
    fn test_replay_fast_forward_to_active() {
        let mut timeline = Timeline::new();
        timeline.push(test_event("A", true));
        timeline.push(test_event("A", false));
        timeline.push(test_event("B", true));
        timeline.push(test_event("B", false));
        timeline.push(test_event("C", true)); // Stalled at C

        let mut engine = ReplayEngine::new(timeline);
        let frame = engine.fast_forward_to_active().unwrap();
        assert_eq!(frame.current_node_id, Some("C".to_string()));
    }

    #[test]
    fn test_replay_with_repeated_nodes() {
        let mut timeline = Timeline::new();
        timeline.push(test_event("A", true));
        timeline.push(test_event("A", false));
        timeline.push(test_event("A", true)); // Stalled at second A

        let mut engine = ReplayEngine::new(timeline);
        let frame = engine.fast_forward_to_active().unwrap();
        assert_eq!(frame.current_node_id, Some("A".to_string()));
        // After fast-forwarding to index 2, calling next_step increments cursor to 3.
        assert_eq!(engine.cursor, 3);
    }
}
