//! Retry, Timeout & Dead Letter Queue Demo
//!
//! ## Purpose
//! Demonstrates Ranvier's built-in retry mechanism with exponential backoff,
//! Dead Letter Queue (DLQ) handling, and application-level timeout patterns.
//!
//! ## Run
//! ```bash
//! cargo run -p retry-dlq-demo
//! ```
//!
//! ## Key Concepts
//! - `DlqPolicy::RetryThenDlq` with configurable max_attempts and backoff
//! - Custom `DlqSink` implementation for dead letter storage
//! - Timeline events: `NodeRetry`, `DlqExhausted`
//! - Application-level circuit breaker pattern with shared state
//! - Timeout wrapping with `tokio::time::timeout`
//!
//! ## Prerequisites
//! - `hello-world` — basic Transition + Axon usage
//! - `custom-error-types` — typed error handling
//!
//! ## Next Steps
//! - `state-persistence-demo` — durable workflow with fault recovery
//! - `observe-http-demo` — observability and tracing integration

use async_trait::async_trait;
use ranvier_core::event::{DlqPolicy, DlqSink};
use ranvier_core::prelude::*;
use ranvier_core::timeline::{Timeline, TimelineEvent};
use ranvier_runtime::Axon;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use tokio::sync::Mutex;

fn local_demo_outcome<T, E>(
    report: ranvier_runtime::ExecutionReport<Outcome<T, E>>,
) -> anyhow::Result<Outcome<T, E>> {
    if report
        .cleanup()
        .is_some_and(|cleanup| !cleanup.is_complete())
    {
        anyhow::bail!("Execution requires reconciliation before retry");
    }
    match report {
        ranvier_runtime::ExecutionReport::Unpersisted { result, .. } => Ok(result),
        _ => anyhow::bail!("Execution requires reconciliation before retry"),
    }
}

// ============================================================================
// Domain Types
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PaymentRequest {
    order_id: String,
    amount: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PaymentResult {
    order_id: String,
    amount: f64,
    status: String,
}

impl PaymentRequest {
    fn is_valid(&self) -> bool {
        !self.order_id.trim().is_empty()
            && self.order_id.len() <= 128
            && !self.order_id.chars().any(char::is_control)
            && self.amount.is_finite()
            && self.amount > 0.0
    }
}

// ============================================================================
// In-Memory DLQ Sink
// ============================================================================

#[derive(Clone, Debug)]
struct DeadLetter {
    workflow: String,
    circuit: String,
    node: String,
    reason: String,
    payload: Vec<u8>,
}

#[derive(Clone)]
struct InMemoryDlqSink {
    letters: Arc<Mutex<Vec<DeadLetter>>>,
}

impl InMemoryDlqSink {
    fn new() -> Self {
        Self {
            letters: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl DlqSink for InMemoryDlqSink {
    async fn store_dead_letter(
        &self,
        workflow_id: &str,
        circuit_label: &str,
        node_id: &str,
        error_msg: &str,
        payload: &[u8],
    ) -> Result<(), String> {
        self.letters.lock().await.push(DeadLetter {
            workflow: workflow_id.into(),
            circuit: circuit_label.into(),
            node: node_id.into(),
            reason: error_msg.into(),
            payload: payload.to_vec(),
        });
        Ok(())
    }
}

// ============================================================================
// Transient Failure Transition (fails N times, then succeeds)
// ============================================================================

#[derive(Clone)]
struct TransientPaymentGateway {
    fail_count: Arc<AtomicU32>,
    failures_before_success: u32,
}

impl TransientPaymentGateway {
    fn new(failures_before_success: u32) -> Self {
        Self {
            fail_count: Arc::new(AtomicU32::new(0)),
            failures_before_success,
        }
    }
}

#[async_trait]
impl Transition<PaymentRequest, PaymentResult> for TransientPaymentGateway {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: PaymentRequest,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<PaymentResult, Self::Error> {
        if !input.is_valid() {
            return Outcome::Fault("invalid_payment_request".into());
        }
        let attempt = self.fail_count.fetch_add(1, Ordering::SeqCst);
        if attempt < self.failures_before_success {
            println!(
                "  [Gateway] Attempt {} FAILED (transient error)",
                attempt + 1
            );
            return Outcome::Fault(format!("gateway timeout (attempt {})", attempt + 1));
        }
        println!("  [Gateway] Attempt {} SUCCEEDED", attempt + 1);
        Outcome::Next(PaymentResult {
            order_id: input.order_id,
            amount: input.amount,
            status: "charged".to_string(),
        })
    }
}

// ============================================================================
// Always-Failing Transition (for DLQ exhaustion demo)
// ============================================================================

#[derive(Clone)]
struct AlwaysFailGateway {
    attempts: Arc<AtomicU32>,
}

#[async_trait]
impl Transition<PaymentRequest, PaymentResult> for AlwaysFailGateway {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: PaymentRequest,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<PaymentResult, Self::Error> {
        if !input.is_valid() {
            return Outcome::Fault("invalid_payment_request".into());
        }
        self.attempts.fetch_add(1, Ordering::SeqCst);
        Outcome::Fault("service_unavailable".to_string())
    }
}

// ============================================================================
// Application-Level Circuit Breaker
// ============================================================================

#[derive(Debug, Clone)]
enum CircuitState {
    Closed { consecutive_failures: u32 },
    Open { opened_at: tokio::time::Instant },
}

#[derive(Clone)]
struct CircuitBreakerGateway {
    state: Arc<Mutex<CircuitState>>,
    failure_threshold: u32,
    reset_timeout: std::time::Duration,
    inner_fail_count: Arc<AtomicU32>,
    inner_failures_before_success: u32,
}

impl CircuitBreakerGateway {
    fn new(
        failure_threshold: u32,
        reset_timeout: std::time::Duration,
        inner_failures_before_success: u32,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(CircuitState::Closed {
                consecutive_failures: 0,
            })),
            failure_threshold,
            reset_timeout,
            inner_fail_count: Arc::new(AtomicU32::new(0)),
            inner_failures_before_success,
        }
    }
}

#[async_trait]
impl Transition<PaymentRequest, PaymentResult> for CircuitBreakerGateway {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: PaymentRequest,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<PaymentResult, Self::Error> {
        if !input.is_valid() {
            return Outcome::Fault("invalid_payment_request".into());
        }
        let mut state = self.state.lock().await;

        // Check circuit state
        match &*state {
            CircuitState::Open { opened_at } => {
                if opened_at.elapsed() < self.reset_timeout {
                    println!("  [CB] Circuit OPEN — rejecting request immediately");
                    return Outcome::Fault("circuit_open".to_string());
                }
                println!("  [CB] Circuit half-open — allowing probe request");
            }
            CircuitState::Closed { .. } => {}
        }

        // Simulate inner call
        let attempt = self.inner_fail_count.fetch_add(1, Ordering::SeqCst);
        if attempt < self.inner_failures_before_success {
            // Failure path
            let failures = match &*state {
                CircuitState::Closed {
                    consecutive_failures,
                } => consecutive_failures + 1,
                _ => 1,
            };

            if matches!(&*state, CircuitState::Open { .. }) || failures >= self.failure_threshold {
                println!("  [CB] Failure — opening circuit");
                *state = CircuitState::Open {
                    opened_at: tokio::time::Instant::now(),
                };
            } else {
                println!(
                    "  [CB] Failure {}/{} — circuit stays closed",
                    failures, self.failure_threshold
                );
                *state = CircuitState::Closed {
                    consecutive_failures: failures,
                };
            }
            return Outcome::Fault(format!("gateway_error (attempt {})", attempt + 1));
        }

        // Success — reset circuit
        println!("  [CB] Success — resetting circuit to closed");
        *state = CircuitState::Closed {
            consecutive_failures: 0,
        };
        Outcome::Next(PaymentResult {
            order_id: input.order_id,
            amount: input.amount,
            status: "charged".to_string(),
        })
    }
}

// ============================================================================
// Timeout-Wrapped Transition
// ============================================================================

#[derive(Clone)]
struct SlowValidation;

#[async_trait]
impl Transition<PaymentRequest, PaymentRequest> for SlowValidation {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: PaymentRequest,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<PaymentRequest, Self::Error> {
        if !input.is_valid() {
            return Outcome::Fault("invalid_payment_request".into());
        }
        // Simulate a slow external validation call
        let timeout_duration = std::time::Duration::from_millis(100);
        let work = async {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            input
        };

        match tokio::time::timeout(timeout_duration, work).await {
            Ok(result) => Outcome::Next(result),
            Err(_) => Outcome::Fault("validation_timeout: exceeded 100ms".to_string()),
        }
    }
}

#[derive(Clone)]
struct FastValidation;

#[async_trait]
impl Transition<PaymentRequest, PaymentRequest> for FastValidation {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: PaymentRequest,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<PaymentRequest, Self::Error> {
        if !input.is_valid() {
            return Outcome::Fault("invalid_payment_request".into());
        }
        let timeout_duration = std::time::Duration::from_millis(100);
        let result = input.clone();
        let work = async {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            result
        };

        match tokio::time::timeout(timeout_duration, work).await {
            Ok(result) => Outcome::Next(result),
            Err(_) => Outcome::Fault("validation_timeout".to_string()),
        }
    }
}

// ============================================================================
// Helper: print timeline summary
// ============================================================================

fn print_timeline(bus: &Bus) {
    if let Ok(timeline) = bus.get_cloned::<Timeline>() {
        let retries: Vec<_> = timeline
            .events
            .iter()
            .filter(|e| matches!(e, TimelineEvent::NodeRetry { .. }))
            .collect();
        let exhausted: Vec<_> = timeline
            .events
            .iter()
            .filter(|e| matches!(e, TimelineEvent::DlqExhausted { .. }))
            .collect();

        if !retries.is_empty() {
            println!("  Timeline: {} retry event(s)", retries.len());
        }
        if !exhausted.is_empty() {
            println!("  Timeline: {} DLQ exhausted event(s)", exhausted.len());
        }
    }
}

// ============================================================================
// Main
// ============================================================================

async fn run_demo() -> anyhow::Result<()> {
    println!("=== Retry, Timeout & DLQ Demo ===\n");

    let dlq = InMemoryDlqSink::new();

    // --- Demo 1: Retry succeeds on 3rd attempt ---
    println!("--- Demo 1: Transient failure with retry recovery ---");
    let mut bus = Bus::new();
    bus.insert(Timeline::new());

    let gateway = TransientPaymentGateway::new(2);
    let axon = Axon::<PaymentRequest, PaymentRequest, String, ()>::new("payment.retry_success")
        .then(gateway.clone())
        .with_dlq_policy(DlqPolicy::RetryThenDlq {
            max_attempts: 5,
            backoff_ms: 1,
        })
        .with_dlq_sink(dlq.clone());

    let request = PaymentRequest {
        order_id: "ORD-001".into(),
        amount: 49.99,
    };
    let result = local_demo_outcome(axon.execute(request, &(), &mut bus).await)?;
    if !matches!(&result, Outcome::Next(value) if value.order_id == "ORD-001" && value.status == "charged" && value.amount == 49.99)
    {
        anyhow::bail!("Retry success mismatch");
    }
    match &result {
        Outcome::Next(r) => println!("  Result: {} ({})", r.status, r.order_id),
        Outcome::Fault(e) => println!("  Result: FAULT — {}", e),
        _ => {}
    }
    print_timeline(&bus);

    if gateway.fail_count.load(Ordering::SeqCst) != 3 || !dlq.letters.lock().await.is_empty() {
        anyhow::bail!("Retry execution count or DLQ mismatch");
    }

    // --- Demo 2: All retries exhausted → DLQ ---
    println!("\n--- Demo 2: All retries exhausted, sent to DLQ ---");
    let mut bus = Bus::new();
    bus.insert(Timeline::new());

    let failures = Arc::new(AtomicU32::new(0));
    let axon = Axon::<PaymentRequest, PaymentRequest, String, ()>::new("payment.retry_exhaust")
        .then(AlwaysFailGateway {
            attempts: failures.clone(),
        })
        .with_dlq_policy(DlqPolicy::RetryThenDlq {
            max_attempts: 3,
            backoff_ms: 1,
        })
        .with_dlq_sink(dlq.clone());

    let request = PaymentRequest {
        order_id: "ORD-002".into(),
        amount: 99.99,
    };
    let result = local_demo_outcome(axon.execute(request, &(), &mut bus).await)?;
    if !matches!(&result, Outcome::Fault(error) if error == "service_unavailable") {
        anyhow::bail!("DLQ exhaustion mismatch");
    }
    match &result {
        Outcome::Fault(e) => println!("  Result: FAULT — {}", e),
        _ => println!("  Result: unexpected"),
    }
    print_timeline(&bus);

    let letters = dlq.letters.lock().await;
    if failures.load(Ordering::SeqCst) != 3
        || letters.len() != 1
        || letters[0].workflow.is_empty()
        || letters[0].node.is_empty()
        || letters[0].circuit != "payment.retry_exhaust"
        || !letters[0].reason.contains("service_unavailable")
        || serde_json::from_slice::<String>(&letters[0].payload)? != "service_unavailable"
    {
        anyhow::bail!("DLQ record mismatch");
    }
    drop(letters);

    // --- Demo 3: Timeout pattern ---
    println!("\n--- Demo 3: Timeout — slow operation exceeds deadline ---");
    let mut bus = Bus::new();
    bus.insert(Timeline::new());

    let axon = Axon::<PaymentRequest, PaymentRequest, String, ()>::new("payment.timeout_fail")
        .then(SlowValidation);

    let request = PaymentRequest {
        order_id: "ORD-003".into(),
        amount: 25.00,
    };
    let result = local_demo_outcome(axon.execute(request, &(), &mut bus).await)?;
    if !matches!(&result, Outcome::Fault(error) if error.starts_with("validation_timeout")) {
        anyhow::bail!("Timeout rejection mismatch");
    }
    match &result {
        Outcome::Fault(e) => println!("  Result: FAULT — {}", e),
        Outcome::Next(_) => println!("  Result: unexpected success"),
        _ => {}
    }

    println!("\n--- Demo 4: Timeout — fast operation completes in time ---");
    let mut bus = Bus::new();
    bus.insert(Timeline::new());

    let axon = Axon::<PaymentRequest, PaymentRequest, String, ()>::new("payment.timeout_ok")
        .then(FastValidation);

    let request = PaymentRequest {
        order_id: "ORD-004".into(),
        amount: 25.00,
    };
    let result = local_demo_outcome(axon.execute(request, &(), &mut bus).await)?;
    if !matches!(&result, Outcome::Next(value) if value.order_id == "ORD-004") {
        anyhow::bail!("Fast validation mismatch");
    }
    match &result {
        Outcome::Next(r) => println!("  Result: validated ({})", r.order_id),
        Outcome::Fault(e) => println!("  Result: FAULT — {}", e),
        _ => {}
    }

    // --- Demo 5: Application-level circuit breaker ---
    println!("\n--- Demo 5: Circuit breaker pattern ---");

    let cb = CircuitBreakerGateway::new(2, std::time::Duration::from_millis(500), 3);

    for i in 1..=5 {
        let mut bus = Bus::new();
        bus.insert(Timeline::new());

        let axon = Axon::<PaymentRequest, PaymentRequest, String, ()>::new("payment.circuit")
            .then(cb.clone());

        let request = PaymentRequest {
            order_id: format!("ORD-CB-{}", i),
            amount: 10.0 * i as f64,
        };

        println!("  Request #{}", i);
        let result = local_demo_outcome(axon.execute(request, &(), &mut bus).await)?;
        if !matches!(&result, Outcome::Fault(error) if if i <= 2 { error.starts_with("gateway_error") } else { error == "circuit_open" })
        {
            anyhow::bail!("Circuit breaker mismatch");
        }
        match &result {
            Outcome::Next(r) => println!("    -> {} ({})", r.status, r.order_id),
            Outcome::Fault(e) => println!("    -> FAULT: {}", e),
            _ => {}
        }
    }

    if cb.inner_fail_count.load(Ordering::SeqCst) != 2 {
        anyhow::bail!("Open breaker invoked suppressed gateway");
    }
    println!("\ndone");
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    run_demo().await
}
#[cfg(test)]
mod tests {
    use super::*;
    async fn expire_open_window(gateway: &CircuitBreakerGateway) {
        let mut state = gateway.state.lock().await;
        assert!(matches!(&*state, CircuitState::Open { .. }));
        *state = CircuitState::Open {
            opened_at: tokio::time::Instant::now()
                .checked_sub(gateway.reset_timeout)
                .unwrap(),
        };
    }

    async fn call_breaker(gateway: CircuitBreakerGateway) -> Outcome<PaymentResult, String> {
        let axon = Axon::<PaymentRequest, PaymentRequest, String, ()>::new("breaker.reset.test")
            .then(gateway);
        local_demo_outcome(
            axon.execute(
                PaymentRequest {
                    order_id: "probe".into(),
                    amount: 1.0,
                },
                &(),
                &mut Bus::new(),
            )
            .await,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn failed_reset_probe_reopens_and_serializes_concurrent_requests() {
        let gateway = CircuitBreakerGateway::new(2, std::time::Duration::from_secs(60), 3);
        for _ in 0..2 {
            assert!(matches!(call_breaker(gateway.clone()).await,
                Outcome::Fault(error) if error.starts_with("gateway_error")));
        }
        expire_open_window(&gateway).await;
        let (first, second) =
            tokio::join!(call_breaker(gateway.clone()), call_breaker(gateway.clone()));
        let results = [first, second];
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result,
            Outcome::Fault(error) if error.starts_with("gateway_error")))
                .count(),
            1
        );
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result,
            Outcome::Fault(error) if error == "circuit_open"))
                .count(),
            1
        );
        assert_eq!(gateway.inner_fail_count.load(Ordering::SeqCst), 3);
        assert!(matches!(
            &*gateway.state.lock().await,
            CircuitState::Open { .. }
        ));
        assert!(matches!(call_breaker(gateway.clone()).await,
            Outcome::Fault(error) if error == "circuit_open"));
        assert_eq!(gateway.inner_fail_count.load(Ordering::SeqCst), 3);
        expire_open_window(&gateway).await;
        assert!(matches!(call_breaker(gateway.clone()).await,
            Outcome::Next(value) if value.order_id == "probe" && value.amount == 1.0));
        assert!(matches!(
            &*gateway.state.lock().await,
            CircuitState::Closed {
                consecutive_failures: 0
            }
        ));
        assert_eq!(gateway.inner_fail_count.load(Ordering::SeqCst), 4);
        assert!(matches!(call_breaker(gateway.clone()).await,
            Outcome::Next(value) if value.status == "charged"));
        assert_eq!(gateway.inner_fail_count.load(Ordering::SeqCst), 5);
    }

    #[tokio::test]
    async fn invalid_requests_do_not_invoke_gateways_or_change_breaker() {
        let transient = TransientPaymentGateway::new(0);
        let attempts = Arc::new(AtomicU32::new(0));
        let always_fail = AlwaysFailGateway {
            attempts: attempts.clone(),
        };
        let breaker = CircuitBreakerGateway::new(2, std::time::Duration::from_secs(60), 3);
        let mut requests: Vec<_> = [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, -1.0]
            .into_iter()
            .map(|amount| PaymentRequest {
                order_id: "valid".into(),
                amount,
            })
            .collect();
        for order_id in [
            "".into(),
            " ".into(),
            "x".repeat(129),
            "bad\nidentifier".into(),
        ] {
            requests.push(PaymentRequest {
                order_id,
                amount: 1.0,
            });
        }
        for request in requests {
            let mut bus = Bus::new();
            assert!(
                matches!(transient.run(request.clone(), &(), &mut bus).await,
                Outcome::Fault(error) if error == "invalid_payment_request")
            );
            assert!(
                matches!(always_fail.run(request.clone(), &(), &mut bus).await,
                Outcome::Fault(error) if error == "invalid_payment_request")
            );
            assert!(matches!(breaker.run(request.clone(), &(), &mut bus).await,
                Outcome::Fault(error) if error == "invalid_payment_request"));
            assert!(
                matches!(SlowValidation.run(request.clone(), &(), &mut bus).await,
                Outcome::Fault(error) if error == "invalid_payment_request")
            );
            assert!(matches!(FastValidation.run(request, &(), &mut bus).await,
                Outcome::Fault(error) if error == "invalid_payment_request"));
        }
        assert_eq!(transient.fail_count.load(Ordering::SeqCst), 0);
        assert_eq!(attempts.load(Ordering::SeqCst), 0);
        assert_eq!(breaker.inner_fail_count.load(Ordering::SeqCst), 0);
        assert!(matches!(
            &*breaker.state.lock().await,
            CircuitState::Closed {
                consecutive_failures: 0
            }
        ));
    }

    #[tokio::test]
    async fn actual_retry_dlq_timeout_and_breaker_scenarios() {
        run_demo().await.unwrap();
    }
}
