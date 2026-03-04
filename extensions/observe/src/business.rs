//! Business metrics helpers for SLI/SLO tracking.
//!
//! # Example
//!
//! ```
//! use ranvier_observe::business::SliTracker;
//! use std::time::Duration;
//!
//! let sli = SliTracker::new("order-service");
//! sli.record_success(Duration::from_millis(45));
//! sli.record_success(Duration::from_millis(120));
//! sli.record_failure(Duration::from_millis(500));
//!
//! assert_eq!(sli.total_requests(), 3);
//! assert_eq!(sli.successful_requests(), 2);
//! assert_eq!(sli.failed_requests(), 1);
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Service Level Indicator tracker.
///
/// Tracks availability (success/failure), latency, and error rate
/// for SLO compliance monitoring.
#[derive(Clone)]
pub struct SliTracker {
    inner: Arc<SliInner>,
}

struct SliInner {
    service_name: String,
    success_count: AtomicU64,
    failure_count: AtomicU64,
    total_latency_us: AtomicU64,
    max_latency_us: AtomicU64,
}

impl SliTracker {
    /// Create a new SLI tracker for the given service.
    pub fn new(service_name: &str) -> Self {
        Self {
            inner: Arc::new(SliInner {
                service_name: service_name.to_string(),
                success_count: AtomicU64::new(0),
                failure_count: AtomicU64::new(0),
                total_latency_us: AtomicU64::new(0),
                max_latency_us: AtomicU64::new(0),
            }),
        }
    }

    /// Record a successful request with its latency.
    #[inline]
    pub fn record_success(&self, latency: Duration) {
        self.inner.success_count.fetch_add(1, Ordering::Relaxed);
        self.record_latency(latency);
    }

    /// Record a failed request with its latency.
    #[inline]
    pub fn record_failure(&self, latency: Duration) {
        self.inner.failure_count.fetch_add(1, Ordering::Relaxed);
        self.record_latency(latency);
    }

    fn record_latency(&self, latency: Duration) {
        let us = latency.as_micros() as u64;
        self.inner.total_latency_us.fetch_add(us, Ordering::Relaxed);
        // Update max latency (CAS loop)
        loop {
            let current = self.inner.max_latency_us.load(Ordering::Relaxed);
            if us <= current {
                break;
            }
            if self
                .inner
                .max_latency_us
                .compare_exchange_weak(current, us, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                break;
            }
        }
    }

    /// Total successful requests.
    pub fn successful_requests(&self) -> u64 {
        self.inner.success_count.load(Ordering::Relaxed)
    }

    /// Total failed requests.
    pub fn failed_requests(&self) -> u64 {
        self.inner.failure_count.load(Ordering::Relaxed)
    }

    /// Total requests (success + failure).
    pub fn total_requests(&self) -> u64 {
        self.successful_requests() + self.failed_requests()
    }

    /// Availability as a ratio (0.0 to 1.0).
    ///
    /// Returns `1.0` if no requests have been recorded.
    pub fn availability(&self) -> f64 {
        let total = self.total_requests();
        if total == 0 {
            return 1.0;
        }
        self.successful_requests() as f64 / total as f64
    }

    /// Error rate as a ratio (0.0 to 1.0).
    pub fn error_rate(&self) -> f64 {
        1.0 - self.availability()
    }

    /// Average latency in milliseconds.
    ///
    /// Returns `0.0` if no requests have been recorded.
    pub fn avg_latency_ms(&self) -> f64 {
        let total = self.total_requests();
        if total == 0 {
            return 0.0;
        }
        let total_us = self.inner.total_latency_us.load(Ordering::Relaxed);
        (total_us as f64 / total as f64) / 1000.0
    }

    /// Maximum observed latency in milliseconds.
    pub fn max_latency_ms(&self) -> f64 {
        self.inner.max_latency_us.load(Ordering::Relaxed) as f64 / 1000.0
    }

    /// Check SLO compliance (availability target).
    ///
    /// Returns `true` if current availability >= target.
    ///
    /// ```
    /// use ranvier_observe::business::SliTracker;
    /// use std::time::Duration;
    ///
    /// let sli = SliTracker::new("api");
    /// for _ in 0..99 {
    ///     sli.record_success(Duration::from_millis(10));
    /// }
    /// sli.record_failure(Duration::from_millis(500));
    ///
    /// assert!(sli.slo_compliant(0.99));  // 99/100 = 0.99, meets 99%
    /// assert!(!sli.slo_compliant(0.999)); // doesn't meet 99.9%
    /// ```
    pub fn slo_compliant(&self, target_availability: f64) -> bool {
        self.availability() >= target_availability
    }

    /// Service name.
    pub fn service_name(&self) -> &str {
        &self.inner.service_name
    }

    /// Render as Prometheus text.
    pub fn render_prometheus(&self) -> String {
        let svc = &self.inner.service_name;
        format!(
            "# HELP sli_{svc}_availability Service availability ratio\n\
             # TYPE sli_{svc}_availability gauge\n\
             sli_{svc}_availability {}\n\
             # HELP sli_{svc}_error_rate Service error rate\n\
             # TYPE sli_{svc}_error_rate gauge\n\
             sli_{svc}_error_rate {}\n\
             # HELP sli_{svc}_requests_total Total requests\n\
             # TYPE sli_{svc}_requests_total counter\n\
             sli_{svc}_requests_total {}\n\
             # HELP sli_{svc}_avg_latency_ms Average latency in ms\n\
             # TYPE sli_{svc}_avg_latency_ms gauge\n\
             sli_{svc}_avg_latency_ms {}\n",
            self.availability(),
            self.error_rate(),
            self.total_requests(),
            self.avg_latency_ms(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sli_basic() {
        let sli = SliTracker::new("test-svc");
        assert_eq!(sli.total_requests(), 0);
        assert_eq!(sli.availability(), 1.0);

        sli.record_success(Duration::from_millis(10));
        sli.record_success(Duration::from_millis(20));
        sli.record_failure(Duration::from_millis(100));

        assert_eq!(sli.total_requests(), 3);
        assert_eq!(sli.successful_requests(), 2);
        assert_eq!(sli.failed_requests(), 1);

        let avail = sli.availability();
        assert!((avail - 0.6667).abs() < 0.01);

        assert!(sli.max_latency_ms() >= 100.0);
    }

    #[test]
    fn slo_compliance() {
        let sli = SliTracker::new("api");
        for _ in 0..999 {
            sli.record_success(Duration::from_millis(5));
        }
        sli.record_failure(Duration::from_millis(500));

        assert!(sli.slo_compliant(0.999)); // 999/1000 = 0.999
        assert!(!sli.slo_compliant(0.9999)); // doesn't meet 99.99%
    }

    #[test]
    fn prometheus_render() {
        let sli = SliTracker::new("orders");
        sli.record_success(Duration::from_millis(50));

        let output = sli.render_prometheus();
        assert!(output.contains("sli_orders_availability 1"));
        assert!(output.contains("sli_orders_requests_total 1"));
    }
}
