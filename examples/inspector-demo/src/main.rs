//! # Inspector Demo
//!
//! Demonstrates the Ranvier Inspector — a built-in runtime observability server
//! that provides real-time metrics, payload capture, stall detection, and
//! conditional breakpoints for Axon circuits.
//!
//! ## Run
//! ```bash
//! cargo run -p inspector-demo
//! ```
//!
//! ## Key APIs
//! - `Inspector::new(schematic, port)` — create an Inspector server
//! - `MetricsCollector` — sliding-window metrics (p50/p95/p99 latency)
//! - `PayloadCapturePolicy` — off / hash / full payload capture
//! - `StallDetector` — threshold-based stall detection
//! - `ConditionalBreakpoint` — JSON path condition evaluator
//!
//! ## Endpoints (dev mode)
//! - `GET  /schematic`         — circuit structure as JSON
//! - `GET  /trace/internal`    — full trace data
//! - `GET  /quick-view`        — built-in dashboard HTML
//! - `GET  /api/v1/metrics`    — per-node latency/throughput metrics
//! - `GET  /api/v1/events`     — captured payload events
//! - `GET  /api/v1/stalls`     — active stall reports
//! - `GET  /api/v1/breakpoints`— conditional breakpoints
//! - `POST /api/v1/breakpoints`— add a conditional breakpoint
//! - `WS   /events`            — real-time event stream
//!
//! ## Try it
//! ```bash
//! # View circuit structure
//! curl http://localhost:9100/schematic | jq
//!
//! # Check metrics
//! curl http://localhost:9100/api/v1/metrics | jq
//!
//! # View captured events
//! curl http://localhost:9100/api/v1/events?limit=10 | jq
//!
//! # Add a conditional breakpoint
//! curl -X POST http://localhost:9100/api/v1/breakpoints \
//!   -H 'Content-Type: application/json' \
//!   -d '{"node_id": "Validate", "condition": "amount > 1000"}'
//!
//! # Check stalls
//! curl http://localhost:9100/api/v1/stalls | jq
//! ```

use async_trait::async_trait;
use ranvier_core::bus::Bus;
use ranvier_core::outcome::Outcome;
use ranvier_core::transition::Transition;
use ranvier_inspector::metrics;
use ranvier_inspector::payload::PayloadCapturePolicy;
use ranvier_inspector::Inspector;
use ranvier_runtime::Axon;
use serde::{Deserialize, Serialize};

// ── Domain types ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OrderRequest {
    order_id: String,
    customer: String,
    amount: f64,
    items: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ValidatedOrder {
    order_id: String,
    customer: String,
    amount: f64,
    items: Vec<String>,
    validated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProcessedOrder {
    order_id: String,
    status: String,
    total: f64,
}

// ── Transitions ───────────────────────────────────────────────────────────

#[derive(Clone)]
struct ValidateOrder;

#[async_trait]
impl Transition<OrderRequest, ValidatedOrder> for ValidateOrder {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: OrderRequest,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<ValidatedOrder, String> {
        // Record metrics for this node
        let start = std::time::Instant::now();

        if input.items.is_empty() {
            metrics::record_global_node_exit("OrderPipeline", "Validate", 0, true);
            return Outcome::Fault("Order has no items".to_string());
        }
        if input.amount <= 0.0 {
            metrics::record_global_node_exit("OrderPipeline", "Validate", 0, true);
            return Outcome::Fault("Invalid amount".to_string());
        }

        let elapsed = start.elapsed().as_millis() as u64;
        metrics::record_global_node_exit("OrderPipeline", "Validate", elapsed, false);

        Outcome::Next(ValidatedOrder {
            order_id: input.order_id,
            customer: input.customer,
            amount: input.amount,
            items: input.items,
            validated: true,
        })
    }
}

#[derive(Clone)]
struct ProcessOrder;

#[async_trait]
impl Transition<ValidatedOrder, ProcessedOrder> for ProcessOrder {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: ValidatedOrder,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<ProcessedOrder, String> {
        let start = std::time::Instant::now();

        // Simulate processing delay
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let elapsed = start.elapsed().as_millis() as u64;
        metrics::record_global_node_exit("OrderPipeline", "Process", elapsed, false);

        Outcome::Next(ProcessedOrder {
            order_id: input.order_id,
            status: "completed".to_string(),
            total: input.amount,
        })
    }
}

// ── Main ──────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Build the Axon circuit
    let axon = Axon::<OrderRequest, OrderRequest, String>::new("OrderPipeline")
        .then(ValidateOrder)
        .then(ProcessOrder);

    // Clone the schematic for Inspector before running
    let schematic = axon.schematic().clone();

    // Start Inspector server on port 9100 (dev mode, payload hash capture)
    let inspector = Inspector::new(schematic, 9100)
        .with_mode("dev")
        .with_auth_enforcement(false)
        .with_payload_capture(PayloadCapturePolicy::Hash)
        .allow_unauthenticated();

    println!("Inspector demo starting...");
    println!("  Inspector UI:  http://localhost:9100/quick-view");
    println!("  Schematic:     http://localhost:9100/schematic");
    println!("  Metrics:       http://localhost:9100/api/v1/metrics");
    println!("  Events:        http://localhost:9100/api/v1/events");
    println!("  Stalls:        http://localhost:9100/api/v1/stalls");
    println!("  Breakpoints:   http://localhost:9100/api/v1/breakpoints");
    println!();

    // Spawn the Inspector server in the background
    tokio::spawn(async move {
        if let Err(e) = inspector.serve().await {
            eprintln!("Inspector server error: {e}");
        }
    });

    // Give the server a moment to start
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Run sample orders to generate metrics and events
    let sample_orders = vec![
        OrderRequest {
            order_id: "ORD-001".into(),
            customer: "Alice".into(),
            amount: 99.99,
            items: vec!["Widget A".into(), "Widget B".into()],
        },
        OrderRequest {
            order_id: "ORD-002".into(),
            customer: "Bob".into(),
            amount: 249.50,
            items: vec!["Gadget X".into()],
        },
        OrderRequest {
            order_id: "ORD-003".into(),
            customer: "Charlie".into(),
            amount: 1500.00,
            items: vec!["Premium Kit".into(), "Add-on Pack".into(), "Support Plan".into()],
        },
    ];

    for order in &sample_orders {
        let mut bus = Bus::new();
        let result = axon.execute(order.clone(), &(), &mut bus).await;
        match &result {
            Outcome::Next(processed) => {
                println!("  Order {} -> {} (${:.2})", processed.order_id, processed.status, processed.total);
            }
            Outcome::Fault(e) => {
                println!("  Order failed: {e}");
            }
            _ => {}
        }
    }

    // Show metrics snapshot
    println!();
    if let Some(snapshot) = metrics::snapshot_circuit("OrderPipeline") {
        println!("Metrics snapshot for '{}':", snapshot.circuit);
        for (node_id, node) in &snapshot.nodes {
            println!(
                "  {node_id}: throughput={}, errors={}, p50={:.1}ms, p95={:.1}ms, p99={:.1}ms",
                node.throughput, node.error_count, node.latency_p50, node.latency_p95, node.latency_p99
            );
        }
    }

    println!();
    println!("Inspector is running. Press Ctrl+C to stop.");
    println!("Try: curl http://localhost:9100/schematic | jq");

    // Keep running for interactive exploration
    tokio::signal::ctrl_c().await?;
    println!("Shutting down.");

    Ok(())
}
