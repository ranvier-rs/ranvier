use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

/// A record of a node that is currently executing.
#[derive(Clone, Debug)]
struct ActiveNode {
    node_id: String,
    circuit: String,
    entered_at: Instant,
}

/// A stalled node report.
#[derive(Clone, Debug, serde::Serialize)]
pub struct StallReport {
    pub node_id: String,
    pub circuit: String,
    pub stalled_ms: u64,
    pub threshold_ms: u64,
}

/// Tracks active node spans and detects stalls based on a configurable threshold.
///
/// DAG circuits cannot deadlock (no cyclic waits), but nodes can stall due to:
/// - External HTTP timeouts
/// - Database lock contention
/// - Infinite retry loops
/// - Abandoned debugger breakpoints
pub struct StallDetector {
    active_nodes: HashMap<String, ActiveNode>,
    threshold_ms: u64,
}

impl StallDetector {
    pub fn new(threshold_ms: u64) -> Self {
        Self {
            active_nodes: HashMap::new(),
            threshold_ms,
        }
    }

    /// Register a node as currently executing.
    pub fn node_entered(&mut self, key: String, node_id: String, circuit: String) {
        self.active_nodes.insert(
            key,
            ActiveNode {
                node_id,
                circuit,
                entered_at: Instant::now(),
            },
        );
    }

    /// Mark a node as no longer executing.
    pub fn node_exited(&mut self, key: &str) {
        self.active_nodes.remove(key);
    }

    /// Check all active nodes and return reports for those exceeding the threshold.
    pub fn detect_stalls(&self) -> Vec<StallReport> {
        let now = Instant::now();
        self.active_nodes
            .values()
            .filter_map(|node| {
                let elapsed = now.duration_since(node.entered_at).as_millis() as u64;
                if elapsed >= self.threshold_ms {
                    Some(StallReport {
                        node_id: node.node_id.clone(),
                        circuit: node.circuit.clone(),
                        stalled_ms: elapsed,
                        threshold_ms: self.threshold_ms,
                    })
                } else {
                    None
                }
            })
            .collect()
    }
}

static STALL_DETECTOR: OnceLock<Arc<Mutex<StallDetector>>> = OnceLock::new();

fn get_stall_detector() -> Arc<Mutex<StallDetector>> {
    let threshold = std::env::var("RANVIER_INSPECTOR_STALL_THRESHOLD_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(30_000);
    STALL_DETECTOR
        .get_or_init(|| Arc::new(Mutex::new(StallDetector::new(threshold))))
        .clone()
}

/// Register a node span as active (called from on_enter).
pub fn register_node(key: String, node_id: String, circuit: String) {
    if let Ok(mut det) = get_stall_detector().lock() {
        det.node_entered(key, node_id, circuit);
    }
}

/// Unregister a node span (called from on_close).
pub fn unregister_node(key: &str) {
    if let Ok(mut det) = get_stall_detector().lock() {
        det.node_exited(key);
    }
}

/// Check for currently stalled nodes.
pub fn detect_stalls() -> Vec<StallReport> {
    get_stall_detector()
        .lock()
        .ok()
        .map(|det| det.detect_stalls())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_stalls_when_below_threshold() {
        let det = StallDetector::new(60_000); // 60s — nothing will stall
        assert!(det.detect_stalls().is_empty());
    }

    #[test]
    fn node_lifecycle() {
        let mut det = StallDetector::new(60_000);
        det.node_entered("span-1".into(), "nodeA".into(), "circuit1".into());
        assert_eq!(det.active_nodes.len(), 1);
        det.node_exited("span-1");
        assert_eq!(det.active_nodes.len(), 0);
    }

    #[test]
    fn stall_detected_with_zero_threshold() {
        let mut det = StallDetector::new(0); // immediate stall
        det.node_entered("span-1".into(), "nodeA".into(), "circuit1".into());
        let stalls = det.detect_stalls();
        assert_eq!(stalls.len(), 1);
        assert_eq!(stalls[0].node_id, "nodeA");
        assert_eq!(stalls[0].circuit, "circuit1");
    }
}
