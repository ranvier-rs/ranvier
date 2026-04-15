use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// A single recorded node execution sample.
#[derive(Clone, Debug)]
struct Sample {
    timestamp_ms: u64,
    duration_ms: u64,
    is_error: bool,
}

/// Sliding-window metrics bucket for a single node.
struct NodeBucket {
    samples: Vec<Sample>,
    window_ms: u64,
}

impl NodeBucket {
    fn new(window_ms: u64) -> Self {
        Self {
            samples: Vec::new(),
            window_ms,
        }
    }

    fn record(&mut self, duration_ms: u64, is_error: bool) {
        let now = epoch_ms();
        self.samples.push(Sample {
            timestamp_ms: now,
            duration_ms,
            is_error,
        });
        self.evict(now);
    }

    fn evict(&mut self, now: u64) {
        let cutoff = now.saturating_sub(self.window_ms);
        self.samples.retain(|s| s.timestamp_ms >= cutoff);
    }

    fn snapshot(&mut self) -> NodeMetricsSnapshot {
        let now = epoch_ms();
        self.evict(now);

        let total = self.samples.len() as u64;
        let errors = self.samples.iter().filter(|s| s.is_error).count() as u64;

        if total == 0 {
            return NodeMetricsSnapshot {
                throughput: 0,
                error_count: 0,
                error_rate: 0.0,
                latency_p50: 0.0,
                latency_p95: 0.0,
                latency_p99: 0.0,
                latency_avg: 0.0,
                sample_count: 0,
            };
        }

        let mut durations: Vec<u64> = self.samples.iter().map(|s| s.duration_ms).collect();
        durations.sort_unstable();

        let window_secs = self.window_ms as f64 / 1000.0;
        let throughput = (total as f64 / window_secs).round() as u64;
        let error_rate = errors as f64 / total as f64;
        let avg = durations.iter().sum::<u64>() as f64 / total as f64;

        NodeMetricsSnapshot {
            throughput,
            error_count: errors,
            error_rate,
            latency_p50: percentile(&durations, 0.50),
            latency_p95: percentile(&durations, 0.95),
            latency_p99: percentile(&durations, 0.99),
            latency_avg: avg,
            sample_count: total,
        }
    }
}

/// Compute percentile from a sorted slice.
fn percentile(sorted: &[u64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0] as f64;
    }
    let rank = p * (sorted.len() - 1) as f64;
    let lower = rank.floor() as usize;
    let upper = rank.ceil() as usize;
    let frac = rank - lower as f64;
    sorted[lower] as f64 * (1.0 - frac) + sorted[upper] as f64 * frac
}

/// Snapshot of metrics for a single node.
#[derive(Clone, Debug, serde::Serialize)]
pub struct NodeMetricsSnapshot {
    pub throughput: u64,
    pub error_count: u64,
    pub error_rate: f64,
    pub latency_p50: f64,
    pub latency_p95: f64,
    pub latency_p99: f64,
    pub latency_avg: f64,
    pub sample_count: u64,
}

/// Snapshot of all node metrics for a circuit.
#[derive(Clone, Debug, serde::Serialize)]
pub struct CircuitMetricsSnapshot {
    pub circuit: String,
    pub window_ms: u64,
    pub nodes: HashMap<String, NodeMetricsSnapshot>,
}

/// Collects per-node execution metrics using a sliding time window.
///
/// Thread-safe: wrapped in Arc<Mutex<>> and shared between InspectorLayer
/// (recording) and Axum handlers (reading).
pub struct MetricsCollector {
    /// node_id -> NodeBucket
    buckets: HashMap<String, NodeBucket>,
    /// circuit name (for snapshot labeling)
    circuit: String,
    /// sliding window duration in ms (default 60_000)
    window_ms: u64,
}

impl MetricsCollector {
    pub fn new(circuit: impl Into<String>, window_ms: u64) -> Self {
        Self {
            buckets: HashMap::new(),
            circuit: circuit.into(),
            window_ms,
        }
    }

    /// Record a node execution completion.
    pub fn record_node_exit(&mut self, node_id: &str, duration_ms: u64, is_error: bool) {
        let bucket = self
            .buckets
            .entry(node_id.to_string())
            .or_insert_with(|| NodeBucket::new(self.window_ms));
        bucket.record(duration_ms, is_error);
    }

    /// Produce a snapshot of all node metrics.
    pub fn snapshot(&mut self) -> CircuitMetricsSnapshot {
        let nodes: HashMap<String, NodeMetricsSnapshot> = self
            .buckets
            .iter_mut()
            .map(|(node_id, bucket)| (node_id.clone(), bucket.snapshot()))
            .collect();
        CircuitMetricsSnapshot {
            circuit: self.circuit.clone(),
            window_ms: self.window_ms,
            nodes,
        }
    }
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Global metrics registry: circuit_name -> MetricsCollector
static METRICS_REGISTRY: std::sync::OnceLock<Arc<Mutex<HashMap<String, MetricsCollector>>>> =
    std::sync::OnceLock::new();

pub fn get_metrics_registry() -> Arc<Mutex<HashMap<String, MetricsCollector>>> {
    METRICS_REGISTRY
        .get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
        .clone()
}

/// Record a node exit in the global metrics registry.
pub fn record_global_node_exit(circuit: &str, node_id: &str, duration_ms: u64, is_error: bool) {
    if let Ok(mut registry) = get_metrics_registry().lock() {
        let collector = registry
            .entry(circuit.to_string())
            .or_insert_with(|| MetricsCollector::new(circuit, 60_000));
        collector.record_node_exit(node_id, duration_ms, is_error);
    }
}

/// Get a snapshot for a specific circuit.
pub fn snapshot_circuit(circuit: &str) -> Option<CircuitMetricsSnapshot> {
    get_metrics_registry()
        .lock()
        .ok()
        .and_then(|mut registry| registry.get_mut(circuit).map(|c| c.snapshot()))
}

/// Get snapshots for all circuits.
pub fn snapshot_all() -> Vec<CircuitMetricsSnapshot> {
    get_metrics_registry()
        .lock()
        .ok()
        .map(|mut registry| registry.iter_mut().map(|(_, c)| c.snapshot()).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_collector_returns_zero_metrics() {
        let mut collector = MetricsCollector::new("test-circuit", 60_000);
        let snap = collector.snapshot();
        assert_eq!(snap.circuit, "test-circuit");
        assert!(snap.nodes.is_empty());
    }

    #[test]
    fn single_sample_produces_valid_percentiles() {
        let mut collector = MetricsCollector::new("test", 60_000);
        collector.record_node_exit("node_a", 42, false);
        let snap = collector.snapshot();
        let node = snap.nodes.get("node_a").unwrap();
        assert_eq!(node.sample_count, 1);
        assert_eq!(node.throughput, 0); // 1 sample / 60s rounds to 0
        assert!((node.latency_p50 - 42.0).abs() < f64::EPSILON);
        assert!((node.latency_p95 - 42.0).abs() < f64::EPSILON);
        assert!((node.latency_p99 - 42.0).abs() < f64::EPSILON);
        assert_eq!(node.error_count, 0);
        assert!((node.error_rate - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn error_rate_computed_correctly() {
        let mut collector = MetricsCollector::new("test", 60_000);
        collector.record_node_exit("node_a", 10, false);
        collector.record_node_exit("node_a", 20, true);
        collector.record_node_exit("node_a", 30, false);
        collector.record_node_exit("node_a", 40, true);
        let snap = collector.snapshot();
        let node = snap.nodes.get("node_a").unwrap();
        assert_eq!(node.sample_count, 4);
        assert_eq!(node.error_count, 2);
        assert!((node.error_rate - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn percentile_interpolation() {
        // 10 samples: 1..=10
        let sorted: Vec<u64> = (1..=10).collect();
        let p50 = percentile(&sorted, 0.50);
        // rank = 0.50 * 9 = 4.5 -> interpolate between index 4 (=5) and 5 (=6)
        assert!((p50 - 5.5).abs() < f64::EPSILON);
        let p95 = percentile(&sorted, 0.95);
        // rank = 0.95 * 9 = 8.55 -> interpolate between 9 (=9) and 9 (=10)
        assert!((p95 - 9.55).abs() < 0.01);
    }

    #[test]
    fn multiple_nodes_tracked_independently() {
        let mut collector = MetricsCollector::new("multi", 60_000);
        collector.record_node_exit("fast", 5, false);
        collector.record_node_exit("slow", 500, false);
        collector.record_node_exit("failing", 100, true);
        let snap = collector.snapshot();
        assert_eq!(snap.nodes.len(), 3);
        assert!((snap.nodes["fast"].latency_p50 - 5.0).abs() < f64::EPSILON);
        assert!((snap.nodes["slow"].latency_p50 - 500.0).abs() < f64::EPSILON);
        assert_eq!(snap.nodes["failing"].error_count, 1);
    }
}
