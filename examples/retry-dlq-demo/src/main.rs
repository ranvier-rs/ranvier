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
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

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

// ============================================================================
// In-Memory DLQ Sink
// ============================================================================

#[derive(Clone)]
struct InMemoryDlqSink {
    letters: Arc<Mutex<Vec<String>>>,
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
        _payload: &[u8],
    ) -> Result<(), String> {
        let entry = format!(
            "workflow={} circuit={} node={} error={}",
            workflow_id, circuit_label, node_id, error_msg
        );
        println!("  [DLQ] Stored dead letter: {}", entry);
        self.letters.lock().await.push(entry);
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
struct AlwaysFailGateway;

#[async_trait]
impl Transition<PaymentRequest, PaymentResult> for AlwaysFailGateway {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        _input: PaymentRequest,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<PaymentResult, Self::Error> {
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

            if failures >= self.failure_threshold {
                println!(
                    "  [CB] Failure threshold reached ({}/{}) — opening circuit",
                    failures, self.failure_threshold
                );
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
    if let Some(timeline) = bus.read::<Timeline>() {
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Retry, Timeout & DLQ Demo ===\n");

    let dlq = InMemoryDlqSink::new();

    // --- Demo 1: Retry succeeds on 3rd attempt ---
    println!("--- Demo 1: Transient failure with retry recovery ---");
    let mut bus = Bus::new();
    bus.insert(Timeline::new());

    let axon = Axon::<PaymentRequest, PaymentRequest, String, ()>::new("payment.retry_success")
        .then(TransientPaymentGateway::new(2))
        .with_dlq_policy(DlqPolicy::RetryThenDlq {
            max_attempts: 5,
            backoff_ms: 1,
        })
        .with_dlq_sink(dlq.clone());

    let request = PaymentRequest {
        order_id: "ORD-001".into(),
        amount: 49.99,
    };
    let result = axon.execute(request, &(), &mut bus).await;
    match &result {
        Outcome::Next(r) => println!("  Result: {} ({})", r.status, r.order_id),
        Outcome::Fault(e) => println!("  Result: FAULT — {}", e),
        _ => {}
    }
    print_timeline(&bus);

    // --- Demo 2: All retries exhausted → DLQ ---
    println!("\n--- Demo 2: All retries exhausted, sent to DLQ ---");
    let mut bus = Bus::new();
    bus.insert(Timeline::new());

    let axon = Axon::<PaymentRequest, PaymentRequest, String, ()>::new("payment.retry_exhaust")
        .then(AlwaysFailGateway)
        .with_dlq_policy(DlqPolicy::RetryThenDlq {
            max_attempts: 3,
            backoff_ms: 1,
        })
        .with_dlq_sink(dlq.clone());

    let request = PaymentRequest {
        order_id: "ORD-002".into(),
        amount: 99.99,
    };
    let result = axon.execute(request, &(), &mut bus).await;
    match &result {
        Outcome::Fault(e) => println!("  Result: FAULT — {}", e),
        _ => println!("  Result: unexpected"),
    }
    print_timeline(&bus);

    let letters = dlq.letters.lock().await;
    println!("  DLQ entries: {}", letters.len());

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
    let result = axon.execute(request, &(), &mut bus).await;
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
    let result = axon.execute(request, &(), &mut bus).await;
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
        let result = axon.execute(request, &(), &mut bus).await;
        match &result {
            Outcome::Next(r) => println!("    -> {} ({})", r.status, r.order_id),
            Outcome::Fault(e) => println!("    -> FAULT: {}", e),
            _ => {}
        }
    }

    println!("\ndone");
    Ok(())
}
