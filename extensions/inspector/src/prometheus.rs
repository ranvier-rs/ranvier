//! Prometheus exposition format renderer for Inspector metrics.
//!
//! Converts [`CircuitMetricsSnapshot`] data into the Prometheus text-based
//! exposition format (v0.0.4).  No external `prometheus` crate dependency —
//! the output is a plain `String`.

use crate::metrics::{self, CircuitMetricsSnapshot};
use std::fmt::Write;

/// Prometheus exposition content-type.
pub const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// Render all circuit metrics as Prometheus exposition text.
pub fn render() -> String {
    let snapshots = metrics::snapshot_all();
    render_snapshots(&snapshots)
}

fn render_snapshots(snapshots: &[CircuitMetricsSnapshot]) -> String {
    let mut out = String::with_capacity(4096);

    // -- per-node invocation count ----------------------------------------
    writeln!(
        out,
        "# HELP ranvier_node_invocations_total Per-node invocation count within the sliding window."
    )
    .ok();
    writeln!(out, "# TYPE ranvier_node_invocations_total gauge").ok();
    for snap in snapshots {
        for (node, m) in &snap.nodes {
            writeln!(
                out,
                "ranvier_node_invocations_total{{circuit=\"{}\",node=\"{}\"}} {}",
                escape(&snap.circuit),
                escape(node),
                m.sample_count,
            )
            .ok();
        }
    }

    // -- per-node error count ---------------------------------------------
    writeln!(out).ok();
    writeln!(
        out,
        "# HELP ranvier_node_errors_total Per-node error count within the sliding window."
    )
    .ok();
    writeln!(out, "# TYPE ranvier_node_errors_total gauge").ok();
    for snap in snapshots {
        for (node, m) in &snap.nodes {
            writeln!(
                out,
                "ranvier_node_errors_total{{circuit=\"{}\",node=\"{}\"}} {}",
                escape(&snap.circuit),
                escape(node),
                m.error_count,
            )
            .ok();
        }
    }

    // -- per-node error rate ----------------------------------------------
    writeln!(out).ok();
    writeln!(
        out,
        "# HELP ranvier_node_error_rate Per-node error rate (0.0–1.0) within the sliding window."
    )
    .ok();
    writeln!(out, "# TYPE ranvier_node_error_rate gauge").ok();
    for snap in snapshots {
        for (node, m) in &snap.nodes {
            writeln!(
                out,
                "ranvier_node_error_rate{{circuit=\"{}\",node=\"{}\"}} {:.6}",
                escape(&snap.circuit),
                escape(node),
                m.error_rate,
            )
            .ok();
        }
    }

    // -- per-node throughput (ops/s) --------------------------------------
    writeln!(out).ok();
    writeln!(
        out,
        "# HELP ranvier_node_throughput Per-node throughput (ops/s) within the sliding window."
    )
    .ok();
    writeln!(out, "# TYPE ranvier_node_throughput gauge").ok();
    for snap in snapshots {
        for (node, m) in &snap.nodes {
            writeln!(
                out,
                "ranvier_node_throughput{{circuit=\"{}\",node=\"{}\"}} {}",
                escape(&snap.circuit),
                escape(node),
                m.throughput,
            )
            .ok();
        }
    }

    // -- per-node latency (milliseconds) ----------------------------------
    writeln!(out).ok();
    writeln!(
        out,
        "# HELP ranvier_node_latency_ms Per-node latency percentiles in milliseconds."
    )
    .ok();
    writeln!(out, "# TYPE ranvier_node_latency_ms gauge").ok();
    for snap in snapshots {
        for (node, m) in &snap.nodes {
            let circuit = escape(&snap.circuit);
            let node_escaped = escape(node);
            writeln!(
                out,
                "ranvier_node_latency_ms{{circuit=\"{circuit}\",node=\"{node_escaped}\",quantile=\"0.5\"}} {:.3}",
                m.latency_p50,
            )
            .ok();
            writeln!(
                out,
                "ranvier_node_latency_ms{{circuit=\"{circuit}\",node=\"{node_escaped}\",quantile=\"0.95\"}} {:.3}",
                m.latency_p95,
            )
            .ok();
            writeln!(
                out,
                "ranvier_node_latency_ms{{circuit=\"{circuit}\",node=\"{node_escaped}\",quantile=\"0.99\"}} {:.3}",
                m.latency_p99,
            )
            .ok();
            writeln!(
                out,
                "ranvier_node_latency_ms{{circuit=\"{circuit}\",node=\"{node_escaped}\",quantile=\"avg\"}} {:.3}",
                m.latency_avg,
            )
            .ok();
        }
    }

    // -- active traces gauge ----------------------------------------------
    writeln!(out).ok();
    writeln!(
        out,
        "# HELP ranvier_active_traces Number of currently active traces."
    )
    .ok();
    writeln!(out, "# TYPE ranvier_active_traces gauge").ok();
    let active_count = crate::get_trace_registry()
        .lock()
        .ok()
        .map(|r| r.active_count())
        .unwrap_or(0);
    writeln!(out, "ranvier_active_traces {active_count}").ok();

    out
}

/// Escape label values per Prometheus spec (backslash, double-quote, newline).
fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::CircuitMetricsSnapshot;
    use crate::metrics::NodeMetricsSnapshot;
    use std::collections::HashMap;

    fn sample_snapshot() -> CircuitMetricsSnapshot {
        let mut nodes = HashMap::new();
        nodes.insert(
            "validate_cart".to_string(),
            NodeMetricsSnapshot {
                throughput: 42,
                error_count: 3,
                error_rate: 0.05,
                latency_p50: 12.5,
                latency_p95: 125.3,
                latency_p99: 250.0,
                latency_avg: 45.2,
                sample_count: 60,
            },
        );
        CircuitMetricsSnapshot {
            circuit: "checkout".to_string(),
            window_ms: 60_000,
            nodes,
        }
    }

    #[test]
    fn render_produces_valid_prometheus_text() {
        let snap = sample_snapshot();
        let output = render_snapshots(&[snap]);

        assert!(output.contains("# HELP ranvier_node_invocations_total"));
        assert!(output.contains("# TYPE ranvier_node_invocations_total gauge"));
        assert!(output.contains(
            "ranvier_node_invocations_total{circuit=\"checkout\",node=\"validate_cart\"} 60"
        ));

        assert!(
            output.contains(
                "ranvier_node_errors_total{circuit=\"checkout\",node=\"validate_cart\"} 3"
            )
        );

        assert!(output.contains(
            "ranvier_node_error_rate{circuit=\"checkout\",node=\"validate_cart\"} 0.050000"
        ));

        assert!(
            output.contains(
                "ranvier_node_throughput{circuit=\"checkout\",node=\"validate_cart\"} 42"
            )
        );

        assert!(output.contains(
            "ranvier_node_latency_ms{circuit=\"checkout\",node=\"validate_cart\",quantile=\"0.5\"} 12.500"
        ));
        assert!(output.contains(
            "ranvier_node_latency_ms{circuit=\"checkout\",node=\"validate_cart\",quantile=\"0.95\"} 125.300"
        ));
        assert!(output.contains(
            "ranvier_node_latency_ms{circuit=\"checkout\",node=\"validate_cart\",quantile=\"0.99\"} 250.000"
        ));

        assert!(output.contains("# HELP ranvier_active_traces"));
        assert!(output.contains("ranvier_active_traces "));
    }

    #[test]
    fn empty_snapshots_produces_headers_only() {
        let output = render_snapshots(&[]);
        assert!(output.contains("# HELP ranvier_node_invocations_total"));
        assert!(output.contains("# HELP ranvier_active_traces"));
        // No data lines for nodes
        assert!(!output.contains("circuit="));
    }

    #[test]
    fn escape_handles_special_chars() {
        assert_eq!(escape("hello"), "hello");
        assert_eq!(escape("a\"b"), "a\\\"b");
        assert_eq!(escape("a\\b"), "a\\\\b");
        assert_eq!(escape("a\nb"), "a\\nb");
    }

    #[test]
    fn content_type_is_prometheus_v004() {
        assert_eq!(CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8");
    }

    #[test]
    fn multi_circuit_renders_all() {
        let mut nodes_a = HashMap::new();
        nodes_a.insert(
            "step1".to_string(),
            NodeMetricsSnapshot {
                throughput: 10,
                error_count: 0,
                error_rate: 0.0,
                latency_p50: 1.0,
                latency_p95: 2.0,
                latency_p99: 3.0,
                latency_avg: 1.5,
                sample_count: 10,
            },
        );
        let snap_a = CircuitMetricsSnapshot {
            circuit: "pipeline_a".into(),
            window_ms: 60_000,
            nodes: nodes_a,
        };

        let mut nodes_b = HashMap::new();
        nodes_b.insert(
            "step2".to_string(),
            NodeMetricsSnapshot {
                throughput: 20,
                error_count: 1,
                error_rate: 0.05,
                latency_p50: 5.0,
                latency_p95: 10.0,
                latency_p99: 15.0,
                latency_avg: 7.0,
                sample_count: 20,
            },
        );
        let snap_b = CircuitMetricsSnapshot {
            circuit: "pipeline_b".into(),
            window_ms: 60_000,
            nodes: nodes_b,
        };

        let output = render_snapshots(&[snap_a, snap_b]);
        assert!(output.contains("circuit=\"pipeline_a\""));
        assert!(output.contains("circuit=\"pipeline_b\""));
        assert!(output.contains("node=\"step1\""));
        assert!(output.contains("node=\"step2\""));
    }

    #[test]
    fn latency_quantile_labels_present() {
        let snap = sample_snapshot();
        let output = render_snapshots(&[snap]);
        assert!(output.contains("quantile=\"0.5\""));
        assert!(output.contains("quantile=\"0.95\""));
        assert!(output.contains("quantile=\"0.99\""));
        assert!(output.contains("quantile=\"avg\""));
    }

    #[test]
    fn help_and_type_lines_count() {
        let output = render_snapshots(&[]);
        let help_count = output.lines().filter(|l| l.starts_with("# HELP")).count();
        let type_count = output.lines().filter(|l| l.starts_with("# TYPE")).count();
        // 6 metric families: invocations, errors, error_rate, throughput, latency, active_traces
        assert_eq!(help_count, 6);
        assert_eq!(type_count, 6);
    }

    #[test]
    fn zero_error_rate_renders_six_decimals() {
        let mut nodes = HashMap::new();
        nodes.insert(
            "ok_node".to_string(),
            NodeMetricsSnapshot {
                throughput: 100,
                error_count: 0,
                error_rate: 0.0,
                latency_p50: 1.0,
                latency_p95: 2.0,
                latency_p99: 3.0,
                latency_avg: 1.5,
                sample_count: 100,
            },
        );
        let snap = CircuitMetricsSnapshot {
            circuit: "clean".into(),
            window_ms: 60_000,
            nodes,
        };
        let output = render_snapshots(&[snap]);
        assert!(
            output.contains("ranvier_node_error_rate{circuit=\"clean\",node=\"ok_node\"} 0.000000")
        );
    }
}
