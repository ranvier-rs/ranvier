use anyhow::Result;
use chrono::{DateTime, TimeZone, Utc};
use ranvier_core::timeline::{Timeline, TimelineEvent};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct TimelineProjectionOptions {
    pub service_name: String,
    pub circuit_id: String,
    pub circuit_version: Option<String>,
    pub trace_id: String,
}

impl TimelineProjectionOptions {
    pub fn new(service_name: impl Into<String>, circuit_id: impl Into<String>) -> Self {
        Self {
            service_name: service_name.into(),
            circuit_id: circuit_id.into(),
            circuit_version: None,
            trace_id: "generated-from-timeline".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProjectionArtifacts {
    pub public_projection: Value,
    pub internal_projection: Value,
}

pub fn projections_from_timeline(
    timeline: &Timeline,
    options: &TimelineProjectionOptions,
) -> Result<ProjectionArtifacts> {
    let mut enter_map: HashMap<String, (String, u64)> = HashMap::new();
    let mut node_rows = Vec::new();
    let mut latencies = Vec::new();
    let mut fault_count = 0u64;
    let mut branch_count = 0u64;
    let mut branch_event_count = 0u64;
    let mut total_count = 0u64;
    let mut min_ts = u64::MAX;
    let mut max_ts = 0u64;

    for event in &timeline.events {
        match event {
            TimelineEvent::NodeEnter {
                node_id,
                node_label,
                timestamp,
            } => {
                enter_map.insert(node_id.clone(), (node_label.clone(), *timestamp));
                min_ts = min_ts.min(*timestamp);
                max_ts = max_ts.max(*timestamp);
            }
            TimelineEvent::NodeExit {
                node_id,
                outcome_type,
                duration_ms,
                timestamp,
            } => {
                let (label, entered) = enter_map
                    .get(node_id)
                    .cloned()
                    .unwrap_or_else(|| ("unknown".to_string(), *timestamp));
                latencies.push(*duration_ms as f64);
                total_count += 1;
                min_ts = min_ts.min(entered);
                max_ts = max_ts.max(*timestamp);

                let descriptor = parse_outcome_descriptor(outcome_type);
                if descriptor.is_fault {
                    fault_count += 1;
                }
                if descriptor.is_branch {
                    branch_count += 1;
                }

                node_rows.push(json!({
                    "node_id": node_id,
                    "label": label,
                    "kind": "Atom",
                    "entered_at": ts_to_rfc3339(entered),
                    "exited_at": ts_to_rfc3339(*timestamp),
                    "latency_ms": *duration_ms as f64,
                    "outcome_type": outcome_type,
                    "branch_id": descriptor.branch_id,
                    "error_code": if descriptor.is_fault { Some("timeline_fault") } else { None::<&str> },
                    "error_category": if descriptor.is_fault { Some("runtime") } else { None::<&str> }
                }));
            }
            TimelineEvent::Branchtaken { .. } => {
                branch_event_count += 1;
            }
        }
    }
    branch_count = branch_count.max(branch_event_count);

    if timeline.events.is_empty() {
        anyhow::bail!("Timeline has no events");
    }

    latencies.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let p95 = percentile(&latencies, 0.95).unwrap_or(0.0);
    let error_rate = if total_count == 0 {
        0.0
    } else {
        fault_count as f64 / total_count as f64
    };
    let success_rate = (1.0 - error_rate).max(0.0);

    let overall_status = if fault_count == 0 {
        "operational"
    } else if error_rate < 0.1 {
        "degraded"
    } else {
        "partial_outage"
    };

    let public_projection = json!({
        "service_name": options.service_name,
        "window_start": ts_to_rfc3339(min_ts),
        "window_end": ts_to_rfc3339(max_ts),
        "overall_status": overall_status,
        "circuits": [{
            "name": options.circuit_id,
            "status": overall_status,
            "success_rate": success_rate,
            "error_rate": error_rate,
            "p95_latency_ms": p95
        }]
    });

    let internal_projection = json!({
        "trace_id": options.trace_id,
        "circuit_id": options.circuit_id,
        "circuit_version": options.circuit_version,
        "started_at": ts_to_rfc3339(min_ts),
        "finished_at": ts_to_rfc3339(max_ts),
        "nodes": node_rows,
        "summary": {
            "node_count": total_count,
            "fault_count": fault_count,
            "branch_count": branch_count
        }
    });

    Ok(ProjectionArtifacts {
        public_projection,
        internal_projection,
    })
}

pub fn write_projection_files(output_dir: &Path, artifacts: &ProjectionArtifacts) -> Result<(PathBuf, PathBuf)> {
    std::fs::create_dir_all(output_dir)?;
    let public_path = output_dir.join("trace.public.json");
    let internal_path = output_dir.join("trace.internal.json");

    std::fs::write(
        &public_path,
        serde_json::to_string_pretty(&artifacts.public_projection)?,
    )?;
    std::fs::write(
        &internal_path,
        serde_json::to_string_pretty(&artifacts.internal_projection)?,
    )?;

    Ok((public_path, internal_path))
}

fn ts_to_rfc3339(ts: u64) -> String {
    let dt: DateTime<Utc> = if ts > 1_000_000_000_000 {
        Utc.timestamp_millis_opt(ts as i64)
            .single()
            .unwrap_or_else(Utc::now)
    } else {
        Utc.timestamp_opt(ts as i64, 0)
            .single()
            .unwrap_or_else(Utc::now)
    };
    dt.to_rfc3339()
}

fn percentile(values: &[f64], p: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let idx = ((values.len() - 1) as f64 * p).round() as usize;
    values.get(idx).copied()
}

struct OutcomeDescriptor {
    is_fault: bool,
    is_branch: bool,
    branch_id: Option<String>,
}

fn parse_outcome_descriptor(outcome: &str) -> OutcomeDescriptor {
    let lowered = outcome.to_ascii_lowercase();
    if lowered.starts_with("branch:") {
        let branch_id = outcome
            .split_once(':')
            .map(|(_, rhs)| rhs.to_string())
            .filter(|s| !s.is_empty());
        return OutcomeDescriptor {
            is_fault: false,
            is_branch: true,
            branch_id,
        };
    }
    OutcomeDescriptor {
        is_fault: lowered.contains("fault") || lowered.contains("error"),
        is_branch: lowered == "branch",
        branch_id: None,
    }
}
