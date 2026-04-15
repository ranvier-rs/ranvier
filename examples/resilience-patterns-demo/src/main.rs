/*!
# Resilience Patterns Demo

## Purpose
Demonstrates Ranvier's built-in resilience capabilities:
- `Axon::then_with_retry()` — automatic retry with configurable backoff
- `Axon::then_with_timeout()` — execution time limit per transition

## Key Concept
Resilience is applied at the **Axon level**, not as standalone Guard nodes.
This is because retry and timeout need to **wrap** a transition's execution,
which Guard nodes (pass-through `Transition<T, T>`) cannot do.

The Axon executor directly manages retry loops and timeout cancellation,
ensuring proper integration with Schematic metadata, Timeline events,
and tracing instrumentation.

## Running
```bash
cargo run -p resilience-patterns-demo
```

## Prerequisites
- `hello-world` — basic Ranvier concepts
- `outcome-variants-demo` — Outcome variant semantics
- `retry-dlq-demo` — DLQ-based retry (alternative pattern)

## Import Note
This example uses workspace crate imports (`ranvier_core`, `ranvier_runtime`, etc.)
because it lives inside the Ranvier workspace. For your own projects, use:
```rust
use ranvier::prelude::*;
```
*/

use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_runtime::{Axon, retry::RetryPolicy};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

// ============================================================================
// Domain Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ApiRequest {
    endpoint: String,
    payload: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ApiResponse {
    status: u16,
    body: String,
    attempts: u32,
}

// ============================================================================
// Scenario 1: then_with_retry — Automatic Retry with Backoff
// ============================================================================

/// Simulates a flaky external API that fails the first N calls.
/// Uses a shared counter to track attempts across retries.
#[derive(Clone)]
struct FlakyApiCall {
    fail_count: Arc<AtomicU32>,
    fail_until: u32,
}

impl FlakyApiCall {
    fn new(fail_until: u32) -> Self {
        Self {
            fail_count: Arc::new(AtomicU32::new(0)),
            fail_until,
        }
    }
}

#[async_trait]
impl Transition<ApiRequest, ApiResponse> for FlakyApiCall {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        request: ApiRequest,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<ApiResponse, Self::Error> {
        let attempt = self.fail_count.fetch_add(1, Ordering::SeqCst) + 1;

        if attempt <= self.fail_until {
            println!(
                "  [FlakyApiCall] Attempt {} for {} -> FAILED (simulated error)",
                attempt, request.endpoint
            );
            return Outcome::Fault(format!("Connection refused (attempt {})", attempt));
        }

        println!(
            "  [FlakyApiCall] Attempt {} for {} -> SUCCESS",
            attempt, request.endpoint
        );
        Outcome::Next(ApiResponse {
            status: 200,
            body: format!("Response from {}", request.endpoint),
            attempts: attempt,
        })
    }
}

// ============================================================================
// Scenario 2: then_with_timeout — Execution Time Limit
// ============================================================================

/// Simulates a slow API call that takes a configurable duration.
#[derive(Clone)]
struct SlowApiCall {
    delay: Duration,
}

#[async_trait]
impl Transition<ApiRequest, ApiResponse> for SlowApiCall {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        request: ApiRequest,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<ApiResponse, Self::Error> {
        println!(
            "  [SlowApiCall] Processing {} (will take {}ms)...",
            request.endpoint,
            self.delay.as_millis()
        );
        tokio::time::sleep(self.delay).await;
        println!("  [SlowApiCall] Done!");

        Outcome::Next(ApiResponse {
            status: 200,
            body: format!("Slow response from {}", request.endpoint),
            attempts: 1,
        })
    }
}

// ============================================================================
// Scenario 3 Helpers: Pass-through transitions for combined pipeline
// ============================================================================

/// Flaky validation (pass-through): fails N times, then passes input unchanged.
#[derive(Clone)]
struct FlakyValidation {
    fail_count: Arc<AtomicU32>,
    fail_until: u32,
}

impl FlakyValidation {
    fn new(fail_until: u32) -> Self {
        Self {
            fail_count: Arc::new(AtomicU32::new(0)),
            fail_until,
        }
    }
}

#[async_trait]
impl Transition<ApiRequest, ApiRequest> for FlakyValidation {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        request: ApiRequest,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<ApiRequest, Self::Error> {
        let attempt = self.fail_count.fetch_add(1, Ordering::SeqCst) + 1;
        if attempt <= self.fail_until {
            println!("  [FlakyValidation] Attempt {} -> FAILED", attempt);
            return Outcome::Fault("Validation service unavailable".to_string());
        }
        println!("  [FlakyValidation] Attempt {} -> passed", attempt);
        Outcome::Next(request)
    }
}

/// Slow response generation with configurable delay.
#[derive(Clone)]
struct SlowRespond {
    delay: Duration,
}

#[async_trait]
impl Transition<ApiRequest, ApiResponse> for SlowRespond {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        request: ApiRequest,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<ApiResponse, Self::Error> {
        println!(
            "  [SlowRespond] Generating response ({}ms delay)...",
            self.delay.as_millis()
        );
        tokio::time::sleep(self.delay).await;
        Outcome::Next(ApiResponse {
            status: 200,
            body: format!("Combined result from {}", request.endpoint),
            attempts: 1,
        })
    }
}

// ============================================================================
// Validation Transition (shared across scenarios)
// ============================================================================

#[derive(Clone)]
struct ValidateRequest;

#[async_trait]
impl Transition<ApiRequest, ApiRequest> for ValidateRequest {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        request: ApiRequest,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<ApiRequest, Self::Error> {
        if request.endpoint.is_empty() {
            return Outcome::Fault("Endpoint cannot be empty".to_string());
        }
        println!("  [ValidateRequest] {} validated", request.endpoint);
        Outcome::Next(request)
    }
}

// ============================================================================
// Main — Run All Scenarios
// ============================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Resilience Patterns Demo ===\n");

    // ── Scenario 1: Retry with exponential backoff ────────────────
    println!("--- Scenario 1: then_with_retry ---");
    println!("    Flaky API fails 2 times, succeeds on 3rd attempt");
    println!("    Policy: max 3 retries, exponential backoff (50ms base)\n");
    {
        let flaky = FlakyApiCall::new(2); // fails first 2 calls
        let pipeline = Axon::<ApiRequest, ApiRequest, String>::new("RetryFlow")
            .then(ValidateRequest)
            .then_with_retry(flaky, RetryPolicy::exponential_default(3, 50));

        let request = ApiRequest {
            endpoint: "/api/orders".into(),
            payload: "{}".into(),
        };
        let mut bus = Bus::new();

        match pipeline.execute(request, &(), &mut bus).await {
            Outcome::Next(resp) => {
                println!(
                    "\n  Result: status={}, attempts={}, body={}",
                    resp.status, resp.attempts, resp.body
                );
            }
            Outcome::Fault(err) => println!("\n  Failed after retries: {}", err),
            other => println!("\n  Unexpected: {:?}", other),
        }
    }

    println!();

    // ── Scenario 2a: Timeout (fast enough) ────────────────────────
    println!("--- Scenario 2a: then_with_timeout (success) ---");
    println!("    API takes 50ms, timeout is 200ms -> succeeds\n");
    {
        let fast = SlowApiCall {
            delay: Duration::from_millis(50),
        };
        let pipeline = Axon::<ApiRequest, ApiRequest, String>::new("TimeoutSuccessFlow")
            .then(ValidateRequest)
            .then_with_timeout(fast, Duration::from_millis(200), || {
                "Request timed out".to_string()
            });

        let request = ApiRequest {
            endpoint: "/api/fast".into(),
            payload: "{}".into(),
        };
        let mut bus = Bus::new();

        match pipeline.execute(request, &(), &mut bus).await {
            Outcome::Next(resp) => {
                println!("  Result: status={}, body={}", resp.status, resp.body);
            }
            Outcome::Fault(err) => println!("  Timeout: {}", err),
            other => println!("  Unexpected: {:?}", other),
        }
    }

    println!();

    // ── Scenario 2b: Timeout (too slow) ───────────────────────────
    println!("--- Scenario 2b: then_with_timeout (timeout) ---");
    println!("    API takes 500ms, timeout is 100ms -> times out\n");
    {
        let slow = SlowApiCall {
            delay: Duration::from_millis(500),
        };
        let pipeline = Axon::<ApiRequest, ApiRequest, String>::new("TimeoutFailFlow")
            .then(ValidateRequest)
            .then_with_timeout(slow, Duration::from_millis(100), || {
                "Request timed out after 100ms".to_string()
            });

        let request = ApiRequest {
            endpoint: "/api/slow".into(),
            payload: "{}".into(),
        };
        let mut bus = Bus::new();

        match pipeline.execute(request, &(), &mut bus).await {
            Outcome::Next(resp) => {
                println!("  Result: status={}, body={}", resp.status, resp.body);
            }
            Outcome::Fault(err) => println!("  Fault caught: {}", err),
            other => println!("  Unexpected: {:?}", other),
        }
    }

    println!();

    // ── Scenario 3: Retry and Timeout in same pipeline ───────────
    println!("--- Scenario 3: Retry + Timeout in same pipeline ---");
    println!("    Step 1 (retried): flaky validation, fails once then succeeds");
    println!("    Step 2 (timeout-guarded): slow response generation, 200ms limit\n");
    {
        // Flaky validation that fails once then passes through
        let flaky_validate = FlakyValidation::new(1);
        let slow_respond = SlowRespond {
            delay: Duration::from_millis(50),
        };

        let pipeline = Axon::<ApiRequest, ApiRequest, String>::new("CombinedFlow")
            .then(ValidateRequest)
            .then_with_retry(
                flaky_validate,
                RetryPolicy::fixed(2, Duration::from_millis(30)),
            )
            .then_with_timeout(slow_respond, Duration::from_millis(200), || {
                "Timed out".to_string()
            });

        let request = ApiRequest {
            endpoint: "/api/combined".into(),
            payload: "{}".into(),
        };
        let mut bus = Bus::new();

        match pipeline.execute(request, &(), &mut bus).await {
            Outcome::Next(resp) => {
                println!("  Result: status={}, body={}", resp.status, resp.body);
            }
            Outcome::Fault(err) => println!("  Final fault: {}", err),
            other => println!("  Unexpected: {:?}", other),
        }
    }

    println!();

    // ── Summary ───────────────────────────────────────────────────
    println!("=== API Reference ===");
    println!("  then_with_retry(transition, policy)");
    println!("    -> Retries on Fault with configurable backoff");
    println!("    -> RetryPolicy::fixed(max, delay)");
    println!("    -> RetryPolicy::exponential(max, initial, multiplier, cap)");
    println!("    -> RetryPolicy::exponential_default(max, initial_ms)");
    println!();
    println!("  then_with_timeout(transition, duration, error_factory)");
    println!("    -> Cancels execution if duration exceeded");
    println!("    -> Returns Fault with user-provided error on timeout");

    Ok(())
}
