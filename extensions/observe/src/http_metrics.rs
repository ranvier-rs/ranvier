use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};
use std::time::{Duration, Instant};

use http::Request;
use tower::{Layer, Service};

const DEFAULT_LATENCY_BUCKETS_MS: [u64; 10] = [5, 10, 25, 50, 100, 250, 500, 1000, 2500, 5000];

/// Snapshot for HTTP request counters and latency histogram.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpMetricsSnapshot {
    pub requests_total: u64,
    pub requests_error: u64,
    pub latency_buckets_ms: Vec<u64>,
    pub latency_bucket_counts: Vec<u64>,
    pub latency_overflow_count: u64,
}

/// In-memory HTTP metrics collector.
#[derive(Clone)]
pub struct HttpMetrics {
    requests_total: Arc<AtomicU64>,
    requests_error: Arc<AtomicU64>,
    latency_buckets_ms: Arc<Vec<u64>>,
    latency_bucket_counts: Arc<Vec<AtomicU64>>,
    latency_overflow_count: Arc<AtomicU64>,
}

impl Default for HttpMetrics {
    fn default() -> Self {
        Self::with_latency_buckets_ms(DEFAULT_LATENCY_BUCKETS_MS.to_vec())
    }
}

impl HttpMetrics {
    pub fn with_latency_buckets_ms(mut buckets: Vec<u64>) -> Self {
        buckets.sort_unstable();
        buckets.dedup();

        let counts = buckets
            .iter()
            .map(|_| AtomicU64::new(0))
            .collect::<Vec<_>>();

        Self {
            requests_total: Arc::new(AtomicU64::new(0)),
            requests_error: Arc::new(AtomicU64::new(0)),
            latency_buckets_ms: Arc::new(buckets),
            latency_bucket_counts: Arc::new(counts),
            latency_overflow_count: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn record(&self, status_code: u16, latency: Duration) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
        if status_code >= 500 {
            self.requests_error.fetch_add(1, Ordering::Relaxed);
        }

        let latency_ms = latency.as_millis() as u64;
        let index = self
            .latency_buckets_ms
            .iter()
            .position(|upper| latency_ms <= *upper);
        if let Some(index) = index {
            self.latency_bucket_counts[index].fetch_add(1, Ordering::Relaxed);
        } else {
            self.latency_overflow_count.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn snapshot(&self) -> HttpMetricsSnapshot {
        let latency_bucket_counts = self
            .latency_bucket_counts
            .iter()
            .map(|count| count.load(Ordering::Relaxed))
            .collect::<Vec<_>>();

        HttpMetricsSnapshot {
            requests_total: self.requests_total.load(Ordering::Relaxed),
            requests_error: self.requests_error.load(Ordering::Relaxed),
            latency_buckets_ms: self.latency_buckets_ms.as_ref().clone(),
            latency_bucket_counts,
            latency_overflow_count: self.latency_overflow_count.load(Ordering::Relaxed),
        }
    }

    /// Render metrics in a Prometheus text exposition style.
    pub fn render_prometheus(&self) -> String {
        let snapshot = self.snapshot();
        let mut lines = vec![
            "# TYPE ranvier_http_requests_total counter".to_string(),
            format!("ranvier_http_requests_total {}", snapshot.requests_total),
            "# TYPE ranvier_http_requests_error_total counter".to_string(),
            format!(
                "ranvier_http_requests_error_total {}",
                snapshot.requests_error
            ),
            "# TYPE ranvier_http_request_latency_bucket counter".to_string(),
        ];

        for (bucket, count) in snapshot
            .latency_buckets_ms
            .iter()
            .zip(snapshot.latency_bucket_counts.iter())
        {
            lines.push(format!(
                "ranvier_http_request_latency_bucket{{le=\"{}\"}} {}",
                bucket, count
            ));
        }

        lines.push(format!(
            "ranvier_http_request_latency_bucket{{le=\"+Inf\"}} {}",
            snapshot.latency_overflow_count
        ));

        lines.join("\n")
    }
}

/// Status extractor trait for layer compatibility with HTTP response types.
pub trait ResponseStatus {
    fn status_code_u16(&self) -> u16;
}

impl<B> ResponseStatus for http::Response<B> {
    fn status_code_u16(&self) -> u16 {
        self.status().as_u16()
    }
}

/// Tower layer that records request counters and latency histogram.
#[derive(Clone)]
pub struct HttpMetricsLayer {
    metrics: HttpMetrics,
}

impl HttpMetricsLayer {
    pub fn new(metrics: HttpMetrics) -> Self {
        Self { metrics }
    }

    pub fn metrics(&self) -> HttpMetrics {
        self.metrics.clone()
    }
}

#[derive(Clone)]
pub struct HttpMetricsService<S> {
    inner: S,
    metrics: HttpMetrics,
}

impl<S> Layer<S> for HttpMetricsLayer {
    type Service = HttpMetricsService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        HttpMetricsService {
            inner,
            metrics: self.metrics.clone(),
        }
    }
}

impl<S, B> Service<Request<B>> for HttpMetricsService<S>
where
    S: Service<Request<B>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Response: ResponseStatus + Send + 'static,
    S::Error: Send + 'static,
    B: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        let mut inner = self.inner.clone();
        let metrics = self.metrics.clone();
        let started_at = Instant::now();

        Box::pin(async move {
            let response = inner.call(req).await;
            let elapsed = started_at.elapsed();
            match &response {
                Ok(resp) => metrics.record(resp.status_code_u16(), elapsed),
                Err(_) => metrics.record(500, elapsed),
            }
            response
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::Response;
    use std::convert::Infallible;
    use tower::{service_fn, Layer, Service};

    #[test]
    fn metrics_snapshot_tracks_counts_and_buckets() {
        let metrics = HttpMetrics::with_latency_buckets_ms(vec![10, 100]);
        metrics.record(200, Duration::from_millis(8));
        metrics.record(503, Duration::from_millis(60));
        metrics.record(200, Duration::from_millis(250));

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.requests_total, 3);
        assert_eq!(snapshot.requests_error, 1);
        assert_eq!(snapshot.latency_bucket_counts, vec![1, 1]);
        assert_eq!(snapshot.latency_overflow_count, 1);

        let rendered = metrics.render_prometheus();
        assert!(rendered.contains("ranvier_http_requests_total 3"));
        assert!(rendered.contains("ranvier_http_requests_error_total 1"));
        assert!(rendered.contains("le=\"10\""));
        assert!(rendered.contains("le=\"+Inf\""));
    }

    #[tokio::test]
    async fn metrics_layer_records_service_response_status_and_latency() {
        let metrics = HttpMetrics::with_latency_buckets_ms(vec![200]);
        let layer = HttpMetricsLayer::new(metrics.clone());
        let mut service = layer.layer(service_fn(|_req: Request<()>| async move {
            tokio::time::sleep(Duration::from_millis(15)).await;
            Ok::<_, Infallible>(Response::builder().status(201).body("ok").unwrap())
        }));

        let request = Request::builder()
            .uri("http://localhost/metrics")
            .body(())
            .expect("request");
        let response = service.call(request).await.expect("response");
        assert_eq!(response.status(), 201);

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.requests_total, 1);
        assert_eq!(snapshot.requests_error, 0);
        assert_eq!(snapshot.latency_bucket_counts, vec![1]);
    }
}
