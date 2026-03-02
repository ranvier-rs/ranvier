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
        // Simple linear scan from the end to find the last NodeEnter without a corresponding NodeExit
        // In a real optimized system, this might use an index maintained during timeline generation.
        let mut active_node = None;
        for i in (0..self.timeline.events.len()).rev() {
            match &self.timeline.events[i] {
                TimelineEvent::NodeEnter { node_id, .. } | TimelineEvent::NodePaused { node_id, .. } => {
                    // Quick check if this node was exited later
                    let mut exited = false;
                    for j in (i + 1)..self.timeline.events.len() {
                        if let TimelineEvent::NodeExit { node_id: exit_id, .. } = &self.timeline.events[j] {
                            if exit_id == node_id {
                                exited = true;
                                break;
                            }
                        }
                    }
                    if !exited {
                        active_node = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }

        if let Some(index) = active_node {
            self.cursor = index;
            self.next_step()
        } else {
            self.fast_forward_to_end()
        }
    }
}
