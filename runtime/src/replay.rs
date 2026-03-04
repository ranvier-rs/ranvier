use crate::persistence::PersistenceStore;
use anyhow::{Result, anyhow};
use ranvier_core::schematic::MigrationRegistry;
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
            TimelineEvent::NodeRetry { node_id, .. } => Some(node_id.clone()),
            TimelineEvent::DlqExhausted { node_id, .. } => Some(node_id.clone()),
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
                TimelineEvent::NodeEnter { node_id, .. }
                | TimelineEvent::NodePaused { node_id, .. } => {
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

/// Result of an event-sourcing replay operation.
#[derive(Debug, Clone)]
pub struct ReplayRecoveryResult {
    /// The trace that was replayed.
    pub trace_id: String,
    /// The original schematic version of the persisted trace.
    pub original_version: String,
    /// The target schematic version after migration.
    pub target_version: String,
    /// Ordered migration hops applied (from → to).
    pub migration_hops: Vec<(String, String)>,
    /// The last known node ID from the persisted events.
    pub last_node_id: Option<String>,
    /// The recovered (and potentially mapped) payload.
    pub recovered_payload: Option<serde_json::Value>,
    /// The step to resume from.
    pub resume_from_step: u64,
}

/// Replays a persisted trace through migration mappers, recovering state
/// for resumption under a newer schematic version.
///
/// This is the core event-sourcing replay function for M172-RQ2. It:
/// 1. Loads the persisted trace from the store
/// 2. Finds a migration path (single or multi-hop) using the registry
/// 3. Applies each migration's payload mapper sequentially
/// 4. Returns a `ReplayRecoveryResult` with the recovered state
pub async fn replay_and_recover(
    store: &dyn PersistenceStore,
    trace_id: &str,
    target_version: &str,
    registry: &MigrationRegistry,
) -> Result<ReplayRecoveryResult> {
    let trace = store
        .load(trace_id)
        .await?
        .ok_or_else(|| anyhow!("trace_id {} not found", trace_id))?;

    let original_version = trace.schematic_version.clone();
    if original_version == target_version {
        // No migration needed — return current state as-is
        let last_event = trace.events.last();
        return Ok(ReplayRecoveryResult {
            trace_id: trace_id.to_string(),
            original_version: original_version.clone(),
            target_version: target_version.to_string(),
            migration_hops: Vec::new(),
            last_node_id: last_event.and_then(|e| e.node_id.clone()),
            recovered_payload: last_event.and_then(|e| e.payload.clone()),
            resume_from_step: last_event.map(|e| e.step.saturating_add(1)).unwrap_or(0),
        });
    }

    let path = registry
        .find_migration_path(&original_version, target_version)
        .ok_or_else(|| {
            anyhow!(
                "no migration path from {} to {} for circuit {}",
                original_version,
                target_version,
                registry.circuit_id
            )
        })?;

    if path.is_empty() {
        return Err(anyhow!(
            "empty migration path from {} to {}",
            original_version,
            target_version
        ));
    }

    // Start with the last persisted payload
    let last_event = trace.events.last();
    let mut current_payload = last_event.and_then(|e| e.payload.clone());
    let last_node_id = last_event.and_then(|e| e.node_id.clone());
    let resume_step = last_event.map(|e| e.step.saturating_add(1)).unwrap_or(0);

    let mut hops = Vec::with_capacity(path.len());

    // Apply each migration hop's payload mapper sequentially
    for migration in &path {
        hops.push((migration.from_version.clone(), migration.to_version.clone()));

        if let (Some(mapper), Some(payload)) = (&migration.payload_mapper, &current_payload) {
            current_payload = Some(mapper.map_state(payload)?);
        }
        // If no mapper, payload passes through unchanged
    }

    Ok(ReplayRecoveryResult {
        trace_id: trace_id.to_string(),
        original_version,
        target_version: target_version.to_string(),
        migration_hops: hops,
        last_node_id,
        recovered_payload: current_payload,
        resume_from_step: resume_step,
    })
}

/// Validates that a migration path exists and all mappers can transform a
/// sample payload without error. Useful as a pre-deployment check.
pub async fn validate_migration_path(
    store: &dyn PersistenceStore,
    trace_id: &str,
    target_version: &str,
    registry: &MigrationRegistry,
) -> Result<bool> {
    match replay_and_recover(store, trace_id, target_version, registry).await {
        Ok(_) => Ok(true),
        Err(e) => {
            tracing::warn!(
                trace_id = %trace_id,
                target_version = %target_version,
                error = %e,
                "Migration path validation failed"
            );
            Ok(false)
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
