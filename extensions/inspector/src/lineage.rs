//! Lineage extraction and trace diff for Inspector.
//!
//! Given a `StoredTrace` with `timeline_json`, extracts the ordered path of
//! nodes that the request traversed. Two lineages can then be structurally
//! compared to reveal path divergence, outcome differences, and latency deltas.

use crate::trace_store::StoredTrace;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------

/// A single node in the execution lineage.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LineageNode {
    pub node_id: String,
    pub outcome_type: Option<String>,
    pub duration_ms: Option<u64>,
}

/// The ordered execution path of a single trace.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Lineage {
    pub trace_id: String,
    pub circuit: String,
    pub nodes: Vec<LineageNode>,
    pub total_duration_ms: u64,
}

/// Result of comparing two lineages.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TraceDiffResult {
    pub trace_a: String,
    pub trace_b: String,
    pub same_path: bool,
    pub nodes_only_in_a: Vec<String>,
    pub nodes_only_in_b: Vec<String>,
    pub outcome_diffs: Vec<OutcomeDiff>,
    pub duration_diffs: Vec<DurationDiff>,
}

/// Outcome difference for a shared node.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OutcomeDiff {
    pub node_id: String,
    pub outcome_a: Option<String>,
    pub outcome_b: Option<String>,
}

/// Duration difference for a shared node.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DurationDiff {
    pub node_id: String,
    pub duration_a_ms: Option<u64>,
    pub duration_b_ms: Option<u64>,
    pub delta_ms: i64,
}

// ---------------------------------------------------------------------------
// Extraction
// ---------------------------------------------------------------------------

/// Extract a lineage from a stored trace's `timeline_json`.
///
/// The expected JSON format is an array of objects with at least a `node_id`
/// field, plus optional `outcome_type` and `duration_ms`:
///
/// ```json
/// [
///   { "node_id": "Validate", "outcome_type": "Next", "duration_ms": 12 },
///   { "node_id": "Process", "outcome_type": "Next", "duration_ms": 45 }
/// ]
/// ```
pub fn extract_lineage(trace: &StoredTrace) -> Option<Lineage> {
    let json_str = trace.timeline_json.as_ref()?;
    let entries: Vec<serde_json::Value> = serde_json::from_str(json_str).ok()?;

    if entries.is_empty() {
        return None;
    }

    let nodes: Vec<LineageNode> = entries
        .iter()
        .filter_map(|entry| {
            let node_id = entry.get("node_id")?.as_str()?.to_string();
            let outcome_type = entry
                .get("outcome_type")
                .and_then(|v| v.as_str())
                .map(String::from);
            let duration_ms = entry.get("duration_ms").and_then(|v| v.as_u64());
            Some(LineageNode {
                node_id,
                outcome_type,
                duration_ms,
            })
        })
        .collect();

    if nodes.is_empty() {
        return None;
    }

    Some(Lineage {
        trace_id: trace.trace_id.clone(),
        circuit: trace.circuit.clone(),
        nodes,
        total_duration_ms: trace.duration_ms,
    })
}

// ---------------------------------------------------------------------------
// Diff
// ---------------------------------------------------------------------------

/// Compare two lineages structurally and temporally.
pub fn diff_traces(a: &Lineage, b: &Lineage) -> TraceDiffResult {
    let ids_a: Vec<&str> = a.nodes.iter().map(|n| n.node_id.as_str()).collect();
    let ids_b: Vec<&str> = b.nodes.iter().map(|n| n.node_id.as_str()).collect();

    let set_a: std::collections::HashSet<&str> = ids_a.iter().copied().collect();
    let set_b: std::collections::HashSet<&str> = ids_b.iter().copied().collect();

    let nodes_only_in_a: Vec<String> = ids_a
        .iter()
        .filter(|id| !set_b.contains(**id))
        .map(|s| s.to_string())
        .collect();
    let nodes_only_in_b: Vec<String> = ids_b
        .iter()
        .filter(|id| !set_a.contains(**id))
        .map(|s| s.to_string())
        .collect();

    let same_path = ids_a == ids_b;

    // Build lookup maps for shared nodes
    let map_a: std::collections::HashMap<&str, &LineageNode> =
        a.nodes.iter().map(|n| (n.node_id.as_str(), n)).collect();
    let map_b: std::collections::HashMap<&str, &LineageNode> =
        b.nodes.iter().map(|n| (n.node_id.as_str(), n)).collect();

    let shared: Vec<&str> = ids_a
        .iter()
        .filter(|id| set_b.contains(**id))
        .copied()
        .collect();

    let mut outcome_diffs = Vec::new();
    let mut duration_diffs = Vec::new();

    for node_id in &shared {
        let na = map_a[node_id];
        let nb = map_b[node_id];

        if na.outcome_type != nb.outcome_type {
            outcome_diffs.push(OutcomeDiff {
                node_id: node_id.to_string(),
                outcome_a: na.outcome_type.clone(),
                outcome_b: nb.outcome_type.clone(),
            });
        }

        if na.duration_ms != nb.duration_ms {
            let da = na.duration_ms.unwrap_or(0) as i64;
            let db = nb.duration_ms.unwrap_or(0) as i64;
            duration_diffs.push(DurationDiff {
                node_id: node_id.to_string(),
                duration_a_ms: na.duration_ms,
                duration_b_ms: nb.duration_ms,
                delta_ms: db - da,
            });
        }
    }

    TraceDiffResult {
        trace_a: a.trace_id.clone(),
        trace_b: b.trace_id.clone(),
        same_path,
        nodes_only_in_a,
        nodes_only_in_b,
        outcome_diffs,
        duration_diffs,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace_store::StoredTrace;

    fn make_stored_trace(id: &str, circuit: &str, timeline: Option<&str>) -> StoredTrace {
        StoredTrace {
            trace_id: id.to_string(),
            circuit: circuit.to_string(),
            status: "completed".to_string(),
            started_at: 1000,
            finished_at: 1100,
            duration_ms: 100,
            outcome_type: Some("Next".to_string()),
            node_count: 3,
            fault_count: 0,
            timeline_json: timeline.map(String::from),
        }
    }

    #[test]
    fn extract_lineage_from_timeline_json() {
        let timeline = r#"[
            {"node_id": "Validate", "outcome_type": "Next", "duration_ms": 12},
            {"node_id": "Process", "outcome_type": "Next", "duration_ms": 45},
            {"node_id": "Respond", "outcome_type": "Next", "duration_ms": 8}
        ]"#;
        let trace = make_stored_trace("t1", "OrderCircuit", Some(timeline));
        let lineage = extract_lineage(&trace).unwrap();

        assert_eq!(lineage.trace_id, "t1");
        assert_eq!(lineage.circuit, "OrderCircuit");
        assert_eq!(lineage.nodes.len(), 3);
        assert_eq!(lineage.nodes[0].node_id, "Validate");
        assert_eq!(lineage.nodes[1].duration_ms, Some(45));
        assert_eq!(lineage.total_duration_ms, 100);
    }

    #[test]
    fn extract_lineage_none_for_missing_timeline() {
        let trace = make_stored_trace("t2", "C", None);
        assert!(extract_lineage(&trace).is_none());
    }

    #[test]
    fn extract_lineage_none_for_malformed_json() {
        let trace = make_stored_trace("t3", "C", Some("not valid json {{{"));
        assert!(extract_lineage(&trace).is_none());
    }

    #[test]
    fn diff_identical_traces() {
        let timeline = r#"[
            {"node_id": "A", "outcome_type": "Next", "duration_ms": 10},
            {"node_id": "B", "outcome_type": "Next", "duration_ms": 20}
        ]"#;
        let t1 = make_stored_trace("t1", "C", Some(timeline));
        let t2 = make_stored_trace("t2", "C", Some(timeline));
        let l1 = extract_lineage(&t1).unwrap();
        let l2 = extract_lineage(&t2).unwrap();

        let diff = diff_traces(&l1, &l2);
        assert!(diff.same_path);
        assert!(diff.nodes_only_in_a.is_empty());
        assert!(diff.nodes_only_in_b.is_empty());
        assert!(diff.outcome_diffs.is_empty());
        assert!(diff.duration_diffs.is_empty());
    }

    #[test]
    fn diff_divergent_paths() {
        let tl_a = r#"[
            {"node_id": "A", "outcome_type": "Next", "duration_ms": 10},
            {"node_id": "B", "outcome_type": "Next", "duration_ms": 20},
            {"node_id": "C", "outcome_type": "Next", "duration_ms": 30}
        ]"#;
        let tl_b = r#"[
            {"node_id": "A", "outcome_type": "Next", "duration_ms": 10},
            {"node_id": "D", "outcome_type": "Error", "duration_ms": 5}
        ]"#;
        let l1 = extract_lineage(&make_stored_trace("t1", "C", Some(tl_a))).unwrap();
        let l2 = extract_lineage(&make_stored_trace("t2", "C", Some(tl_b))).unwrap();

        let diff = diff_traces(&l1, &l2);
        assert!(!diff.same_path);
        assert_eq!(diff.nodes_only_in_a, vec!["B", "C"]);
        assert_eq!(diff.nodes_only_in_b, vec!["D"]);
    }

    #[test]
    fn diff_outcome_differences() {
        let tl_a = r#"[
            {"node_id": "A", "outcome_type": "Next", "duration_ms": 10}
        ]"#;
        let tl_b = r#"[
            {"node_id": "A", "outcome_type": "Error", "duration_ms": 10}
        ]"#;
        let l1 = extract_lineage(&make_stored_trace("t1", "C", Some(tl_a))).unwrap();
        let l2 = extract_lineage(&make_stored_trace("t2", "C", Some(tl_b))).unwrap();

        let diff = diff_traces(&l1, &l2);
        assert!(diff.same_path);
        assert_eq!(diff.outcome_diffs.len(), 1);
        assert_eq!(diff.outcome_diffs[0].node_id, "A");
        assert_eq!(diff.outcome_diffs[0].outcome_a, Some("Next".to_string()));
        assert_eq!(diff.outcome_diffs[0].outcome_b, Some("Error".to_string()));
    }

    #[test]
    fn diff_duration_differences() {
        let tl_a = r#"[
            {"node_id": "A", "outcome_type": "Next", "duration_ms": 10},
            {"node_id": "B", "outcome_type": "Next", "duration_ms": 50}
        ]"#;
        let tl_b = r#"[
            {"node_id": "A", "outcome_type": "Next", "duration_ms": 10},
            {"node_id": "B", "outcome_type": "Next", "duration_ms": 80}
        ]"#;
        let l1 = extract_lineage(&make_stored_trace("t1", "C", Some(tl_a))).unwrap();
        let l2 = extract_lineage(&make_stored_trace("t2", "C", Some(tl_b))).unwrap();

        let diff = diff_traces(&l1, &l2);
        assert!(diff.same_path);
        assert!(diff.outcome_diffs.is_empty());
        assert_eq!(diff.duration_diffs.len(), 1);
        assert_eq!(diff.duration_diffs[0].node_id, "B");
        assert_eq!(diff.duration_diffs[0].delta_ms, 30); // 80 - 50
    }
}
