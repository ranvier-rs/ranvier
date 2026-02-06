use ranvier_core::prelude::*;
use ranvier_core::schematic::Schematic;
use ranvier_core::timeline::{Timeline, TimelineEvent};
use ranvier_macros::transition;
use ranvier_runtime::Axon;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

#[transition]
async fn step_one(input: i32) -> Outcome<i32, anyhow::Error> {
    Outcome::Next(input + 10)
}

#[transition]
async fn step_two(input: i32) -> Outcome<String, anyhow::Error> {
    Outcome::Next(format!("Result: {}", input))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use tracing_subscriber::prelude::*;

    let fmt_layer = tracing_subscriber::fmt::layer();
    let inspector_layer = ranvier_inspector::layer();

    tracing_subscriber::registry()
        .with(fmt_layer)
        .with(inspector_layer)
        .init();

    tracing::info!("Starting Studio Demo...");

    // Start with i32 -> i32 identity
    let info_axon = Axon::<i32, i32, anyhow::Error>::start("Studio Demo Circuit")
        .then(step_one)
        .then(step_two);

    // Configure default local artifact paths for "run once and inspect" workflow.
    let dist_dir = PathBuf::from("./dist/studio-demo");
    fs::create_dir_all(&dist_dir)?;
    let timeline_path = dist_dir.join("timeline.raw.json");
    let public_path = dist_dir.join("trace.public.json");
    let internal_path = dist_dir.join("trace.internal.json");

    set_env_if_missing("RANVIER_TIMELINE_OUTPUT", timeline_path.display().to_string());
    set_env_if_missing("RANVIER_TIMELINE_MODE", "overwrite".to_string());
    set_env_if_missing(
        "RANVIER_TRACE_PUBLIC_PATH",
        public_path.display().to_string(),
    );
    set_env_if_missing(
        "RANVIER_TRACE_INTERNAL_PATH",
        internal_path.display().to_string(),
    );

    let axon = info_axon.serve_inspector(9000);

    tracing::info!("Inspector mode: RANVIER_MODE=dev|prod, enabled by RANVIER_INSPECTOR=1|0");
    tracing::info!("Inspector dev page: http://localhost:9000/quick-view");
    tracing::info!("Raw endpoints: /schematic, /trace/public, /trace/internal (dev only)");
    tracing::info!(
        "Projection artifacts: {}, {}",
        public_path.display(),
        internal_path.display()
    );

    loop {
        tracing::info!("Executing Axon...");
        let _ = axon.execute(50, &(), &mut Bus::new()).await;
        if let Err(err) = regenerate_projection_from_timeline(
            &timeline_path,
            &public_path,
            &internal_path,
            axon.schematic(),
        ) {
            tracing::warn!("Projection refresh failed: {}", err);
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

fn set_env_if_missing(key: &str, value: String) {
    if std::env::var_os(key).is_none() {
        // Safe in this single-threaded startup path before background worker spawn.
        unsafe {
            std::env::set_var(key, value);
        }
    }
}

fn regenerate_projection_from_timeline(
    timeline_path: &PathBuf,
    public_path: &PathBuf,
    internal_path: &PathBuf,
    schematic: &Schematic,
) -> anyhow::Result<()> {
    if !timeline_path.exists() {
        return Ok(());
    }

    let content = fs::read_to_string(timeline_path)?;
    let timeline: Timeline = serde_json::from_str(&content)?;
    if timeline.events.is_empty() {
        return Ok(());
    }

    let mut enter_map: HashMap<String, (String, u64)> = HashMap::new();
    let mut node_rows = Vec::new();
    let mut latencies = Vec::new();
    let mut fault_count = 0u64;
    let mut branch_count = 0u64;
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
                    .unwrap_or_else(|| (node_id.clone(), *timestamp));

                total_count += 1;
                latencies.push(*duration_ms as f64);
                min_ts = min_ts.min(entered);
                max_ts = max_ts.max(*timestamp);

                let lowered = outcome_type.to_ascii_lowercase();
                if lowered.contains("fault") || lowered.contains("error") {
                    fault_count += 1;
                }
                if lowered.starts_with("branch:") {
                    branch_count += 1;
                }

                node_rows.push(serde_json::json!({
                    "node_id": node_id,
                    "label": label,
                    "kind": "Atom",
                    "entered_at": ts_to_rfc3339(entered),
                    "exited_at": ts_to_rfc3339(*timestamp),
                    "latency_ms": *duration_ms as f64,
                    "outcome_type": outcome_type,
                    "branch_id": outcome_type.split_once(':').map(|(_, rhs)| rhs.to_string()),
                    "error_code": if lowered.contains("fault") || lowered.contains("error") { Some("runtime_fault") } else { None::<&str> },
                    "error_category": if lowered.contains("fault") || lowered.contains("error") { Some("runtime") } else { None::<&str> }
                }));
            }
            TimelineEvent::Branchtaken { .. } => {
                branch_count += 1;
            }
        }
    }

    let p95 = percentile(&latencies, 0.95).unwrap_or(0.0);
    let error_rate = if total_count == 0 {
        0.0
    } else {
        fault_count as f64 / total_count as f64
    };
    let success_rate = (1.0 - error_rate).max(0.0);
    let status = if fault_count == 0 {
        "operational"
    } else if error_rate < 0.1 {
        "degraded"
    } else {
        "partial_outage"
    };

    let public_projection = serde_json::json!({
        "service_name": schematic.name,
        "window_start": ts_to_rfc3339(min_ts),
        "window_end": ts_to_rfc3339(max_ts),
        "overall_status": status,
        "circuits": [{
            "name": schematic.name,
            "status": status,
            "success_rate": success_rate,
            "error_rate": error_rate,
            "p95_latency_ms": p95
        }]
    });

    let internal_projection = serde_json::json!({
        "trace_id": "studio-demo-live",
        "circuit_id": schematic.id,
        "started_at": ts_to_rfc3339(min_ts),
        "finished_at": ts_to_rfc3339(max_ts),
        "nodes": node_rows,
        "summary": {
            "node_count": total_count,
            "fault_count": fault_count,
            "branch_count": branch_count
        }
    });

    fs::write(public_path, serde_json::to_string_pretty(&public_projection)?)?;
    fs::write(internal_path, serde_json::to_string_pretty(&internal_projection)?)?;
    Ok(())
}

fn ts_to_rfc3339(ts: u64) -> String {
    use chrono::{TimeZone, Utc};
    let dt = if ts > 1_000_000_000_000 {
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
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted.get(idx).copied()
}
