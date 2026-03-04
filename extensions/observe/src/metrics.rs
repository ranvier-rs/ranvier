//! Custom metrics API for Ranvier applications.
//!
//! Provides thread-safe `Counter`, `Gauge`, and `Histogram` types
//! that can be shared across Tower layers, Axon transitions, and HTTP handlers.
//!
//! # Example
//!
//! ```
//! use ranvier_observe::metrics::{Counter, Gauge, Histogram, MetricsRegistry};
//!
//! let registry = MetricsRegistry::new();
//!
//! let requests = Counter::new("http_requests_total", "Total HTTP requests");
//! let active = Gauge::new("active_connections", "Active connections");
//! let latency = Histogram::new("request_duration_ms", "Request latency in ms");
//!
//! registry.register_counter(requests.clone());
//! registry.register_gauge(active.clone());
//! registry.register_histogram(latency.clone());
//!
//! requests.inc();
//! requests.inc_by(5);
//! active.set(42.0);
//! latency.observe(12.5);
//!
//! let output = registry.render_prometheus();
//! assert!(output.contains("http_requests_total"));
//! ```

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Counter
// ---------------------------------------------------------------------------

/// Monotonically increasing counter.
///
/// Thread-safe and cloneable. Supports optional key-value labels.
#[derive(Clone)]
pub struct Counter {
    inner: Arc<CounterInner>,
}

struct CounterInner {
    name: String,
    help: String,
    /// Unlabeled counter
    value: AtomicU64,
    /// Labeled counters: label_key=label_value → count
    labeled: Mutex<BTreeMap<Vec<(String, String)>, AtomicU64Wrapper>>,
}

struct AtomicU64Wrapper(AtomicU64);

impl Counter {
    /// Create a new counter with the given name and help text.
    pub fn new(name: &str, help: &str) -> Self {
        Self {
            inner: Arc::new(CounterInner {
                name: name.to_string(),
                help: help.to_string(),
                value: AtomicU64::new(0),
                labeled: Mutex::new(BTreeMap::new()),
            }),
        }
    }

    /// Increment by 1.
    #[inline]
    pub fn inc(&self) {
        self.inner.value.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment by the given amount.
    #[inline]
    pub fn inc_by(&self, n: u64) {
        self.inner.value.fetch_add(n, Ordering::Relaxed);
    }

    /// Increment a labeled counter.
    pub fn inc_with(&self, labels: &[(&str, &str)]) {
        let key: Vec<(String, String)> = labels
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let mut map = self.inner.labeled.lock().expect("counter labels lock");
        map.entry(key)
            .or_insert_with(|| AtomicU64Wrapper(AtomicU64::new(0)))
            .0
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Get the current value.
    #[inline]
    pub fn get(&self) -> u64 {
        self.inner.value.load(Ordering::Relaxed)
    }

    /// Name of this counter.
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// Help text.
    pub fn help(&self) -> &str {
        &self.inner.help
    }

    /// Render as Prometheus text.
    pub fn render_prometheus(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("# HELP {} {}\n", self.inner.name, self.inner.help));
        out.push_str(&format!("# TYPE {} counter\n", self.inner.name));
        out.push_str(&format!("{} {}\n", self.inner.name, self.get()));

        let map = self.inner.labeled.lock().expect("counter labels lock");
        for (labels, value) in map.iter() {
            let label_str = labels
                .iter()
                .map(|(k, v)| format!("{}=\"{}\"", k, v))
                .collect::<Vec<_>>()
                .join(",");
            out.push_str(&format!(
                "{}{{{}}} {}\n",
                self.inner.name,
                label_str,
                value.0.load(Ordering::Relaxed)
            ));
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Gauge
// ---------------------------------------------------------------------------

/// Gauge that can go up and down (stored as f64 via AtomicU64 bit-casting).
#[derive(Clone)]
pub struct Gauge {
    inner: Arc<GaugeInner>,
}

struct GaugeInner {
    name: String,
    help: String,
    value: AtomicU64, // stores f64 bits
}

impl Gauge {
    /// Create a new gauge with the given name and help text.
    pub fn new(name: &str, help: &str) -> Self {
        Self {
            inner: Arc::new(GaugeInner {
                name: name.to_string(),
                help: help.to_string(),
                value: AtomicU64::new(f64::to_bits(0.0)),
            }),
        }
    }

    /// Set the gauge to the given value.
    #[inline]
    pub fn set(&self, val: f64) {
        self.inner.value.store(f64::to_bits(val), Ordering::Relaxed);
    }

    /// Increment the gauge by the given amount.
    pub fn inc(&self, delta: f64) {
        loop {
            let current = self.inner.value.load(Ordering::Relaxed);
            let new = f64::from_bits(current) + delta;
            if self
                .inner
                .value
                .compare_exchange_weak(
                    current,
                    f64::to_bits(new),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                break;
            }
        }
    }

    /// Decrement the gauge by the given amount.
    pub fn dec(&self, delta: f64) {
        self.inc(-delta);
    }

    /// Get the current value.
    #[inline]
    pub fn get(&self) -> f64 {
        f64::from_bits(self.inner.value.load(Ordering::Relaxed))
    }

    /// Name of this gauge.
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// Help text.
    pub fn help(&self) -> &str {
        &self.inner.help
    }

    /// Render as Prometheus text.
    pub fn render_prometheus(&self) -> String {
        format!(
            "# HELP {} {}\n# TYPE {} gauge\n{} {}\n",
            self.inner.name,
            self.inner.help,
            self.inner.name,
            self.inner.name,
            self.get()
        )
    }
}

// ---------------------------------------------------------------------------
// Histogram
// ---------------------------------------------------------------------------

/// Bucket-based histogram for latency/value distributions.
#[derive(Clone)]
pub struct Histogram {
    inner: Arc<HistogramInner>,
}

struct HistogramInner {
    name: String,
    help: String,
    buckets: Vec<f64>,
    counts: Vec<AtomicU64>,
    sum: AtomicU64, // f64 bits
    count: AtomicU64,
}

impl Histogram {
    /// Create with default latency buckets: [5, 10, 25, 50, 100, 250, 500, 1000, 2500, 5000] ms.
    pub fn new(name: &str, help: &str) -> Self {
        Self::with_buckets(
            name,
            help,
            &[
                5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2500.0, 5000.0,
            ],
        )
    }

    /// Create with custom bucket boundaries.
    pub fn with_buckets(name: &str, help: &str, buckets: &[f64]) -> Self {
        let counts = buckets.iter().map(|_| AtomicU64::new(0)).collect();
        Self {
            inner: Arc::new(HistogramInner {
                name: name.to_string(),
                help: help.to_string(),
                buckets: buckets.to_vec(),
                counts,
                sum: AtomicU64::new(f64::to_bits(0.0)),
                count: AtomicU64::new(0),
            }),
        }
    }

    /// Record an observation.
    #[inline]
    pub fn observe(&self, value: f64) {
        self.inner.count.fetch_add(1, Ordering::Relaxed);
        // CAS loop for sum
        loop {
            let current = self.inner.sum.load(Ordering::Relaxed);
            let new = f64::from_bits(current) + value;
            if self
                .inner
                .sum
                .compare_exchange_weak(
                    current,
                    f64::to_bits(new),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                break;
            }
        }
        for (i, boundary) in self.inner.buckets.iter().enumerate() {
            if value <= *boundary {
                self.inner.counts[i].fetch_add(1, Ordering::Relaxed);
                break;
            }
        }
    }

    /// Get total observation count.
    pub fn count(&self) -> u64 {
        self.inner.count.load(Ordering::Relaxed)
    }

    /// Get sum of all observations.
    pub fn sum(&self) -> f64 {
        f64::from_bits(self.inner.sum.load(Ordering::Relaxed))
    }

    /// Name of this histogram.
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// Help text.
    pub fn help(&self) -> &str {
        &self.inner.help
    }

    /// Render as Prometheus text.
    pub fn render_prometheus(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("# HELP {} {}\n", self.inner.name, self.inner.help));
        out.push_str(&format!("# TYPE {} histogram\n", self.inner.name));

        let mut cumulative = 0u64;
        for (i, boundary) in self.inner.buckets.iter().enumerate() {
            cumulative += self.inner.counts[i].load(Ordering::Relaxed);
            out.push_str(&format!(
                "{}_bucket{{le=\"{}\"}} {}\n",
                self.inner.name, boundary, cumulative
            ));
        }
        out.push_str(&format!(
            "{}_bucket{{le=\"+Inf\"}} {}\n",
            self.inner.name,
            self.count()
        ));
        out.push_str(&format!("{}_sum {}\n", self.inner.name, self.sum()));
        out.push_str(&format!("{}_count {}\n", self.inner.name, self.count()));
        out
    }
}

// ---------------------------------------------------------------------------
// MetricsRegistry
// ---------------------------------------------------------------------------

/// Global registry that collects all custom metrics for rendering.
#[derive(Clone, Default)]
pub struct MetricsRegistry {
    counters: Arc<Mutex<Vec<Counter>>>,
    gauges: Arc<Mutex<Vec<Gauge>>>,
    histograms: Arc<Mutex<Vec<Histogram>>>,
}

impl MetricsRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a counter.
    pub fn register_counter(&self, counter: Counter) {
        self.counters.lock().expect("registry lock").push(counter);
    }

    /// Register a gauge.
    pub fn register_gauge(&self, gauge: Gauge) {
        self.gauges.lock().expect("registry lock").push(gauge);
    }

    /// Register a histogram.
    pub fn register_histogram(&self, histogram: Histogram) {
        self.histograms
            .lock()
            .expect("registry lock")
            .push(histogram);
    }

    /// Render all registered metrics as Prometheus text format.
    pub fn render_prometheus(&self) -> String {
        let mut out = String::new();
        for c in self.counters.lock().expect("registry lock").iter() {
            out.push_str(&c.render_prometheus());
            out.push('\n');
        }
        for g in self.gauges.lock().expect("registry lock").iter() {
            out.push_str(&g.render_prometheus());
            out.push('\n');
        }
        for h in self.histograms.lock().expect("registry lock").iter() {
            out.push_str(&h.render_prometheus());
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_basic() {
        let c = Counter::new("test_counter", "A test counter");
        assert_eq!(c.get(), 0);
        c.inc();
        assert_eq!(c.get(), 1);
        c.inc_by(9);
        assert_eq!(c.get(), 10);
    }

    #[test]
    fn counter_labeled() {
        let c = Counter::new("labeled_counter", "Labeled");
        c.inc_with(&[("method", "GET"), ("status", "200")]);
        c.inc_with(&[("method", "GET"), ("status", "200")]);
        c.inc_with(&[("method", "POST"), ("status", "201")]);

        let prom = c.render_prometheus();
        assert!(prom.contains("labeled_counter{method=\"GET\",status=\"200\"} 2"));
        assert!(prom.contains("labeled_counter{method=\"POST\",status=\"201\"} 1"));
    }

    #[test]
    fn gauge_basic() {
        let g = Gauge::new("test_gauge", "A test gauge");
        assert_eq!(g.get(), 0.0);
        g.set(42.0);
        assert_eq!(g.get(), 42.0);
        g.inc(8.0);
        assert_eq!(g.get(), 50.0);
        g.dec(10.0);
        assert_eq!(g.get(), 40.0);
    }

    #[test]
    fn histogram_basic() {
        let h = Histogram::with_buckets("test_hist", "A test histogram", &[10.0, 50.0, 100.0]);
        h.observe(5.0);
        h.observe(25.0);
        h.observe(75.0);
        h.observe(200.0);

        assert_eq!(h.count(), 4);
        assert!((h.sum() - 305.0).abs() < 0.001);

        let prom = h.render_prometheus();
        assert!(prom.contains("test_hist_bucket{le=\"10\"} 1"));
        assert!(prom.contains("test_hist_bucket{le=\"50\"} 2"));
        assert!(prom.contains("test_hist_bucket{le=\"100\"} 3"));
        assert!(prom.contains("test_hist_bucket{le=\"+Inf\"} 4"));
    }

    #[test]
    fn registry_render_all() {
        let reg = MetricsRegistry::new();
        let c = Counter::new("req_total", "Total requests");
        let g = Gauge::new("active_conns", "Active connections");
        let h = Histogram::new("latency_ms", "Latency");

        c.inc_by(100);
        g.set(5.0);
        h.observe(42.0);

        reg.register_counter(c);
        reg.register_gauge(g);
        reg.register_histogram(h);

        let output = reg.render_prometheus();
        assert!(output.contains("req_total 100"));
        assert!(output.contains("active_conns 5"));
        assert!(output.contains("latency_ms_count 1"));
    }
}
