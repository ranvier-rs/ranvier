//! Testing Patterns Example
//!
//! ## Purpose
//! Demonstrates comprehensive testing strategies for Ranvier Transitions and Axons,
//! including unit tests, integration tests, error handling, and mock resources.
//!
//! ## Run
//! ```bash
//! cargo run -p testing-patterns
//! cargo test -p testing-patterns
//! ```
//!
//! ## Key Concepts
//! - Unit testing individual Transitions with `.run()`
//! - Integration testing full Axon chains with `.execute()`
//! - Testing Outcome variants (Next, Fault)
//! - Isolated transition tests without Axon overhead
//!
//! ## Prerequisites
//! - `hello-world` — basic Transition + Axon usage
//!
//! ## Next Steps
//! - `custom-error-types` — domain-specific error handling
//! - `retry-dlq-demo` — retry, timeout, and DLQ patterns

use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_runtime::Axon;
use serde::{Deserialize, Serialize};

// ============================================================================
// Domain Types
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
struct OrderRequest {
    item: String,
    quantity: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ValidatedOrder {
    item: String,
    quantity: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ProcessedOrder {
    order_id: String,
    item: String,
    quantity: u32,
    total: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct OrderConfirmation {
    message: String,
}

// ============================================================================
// Transitions
// ============================================================================

#[derive(Clone)]
struct ValidateInput;

#[async_trait]
impl Transition<OrderRequest, ValidatedOrder> for ValidateInput {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: OrderRequest,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<ValidatedOrder, Self::Error> {
        if input.item.is_empty() {
            return Outcome::Fault("Item name cannot be empty".to_string());
        }
        if input.quantity == 0 {
            return Outcome::Fault("Quantity must be greater than 0".to_string());
        }

        Outcome::Next(ValidatedOrder {
            item: input.item,
            quantity: input.quantity,
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
    ) -> Outcome<ProcessedOrder, Self::Error> {
        let order_id = format!("ORD-{}", rand_id());
        let total = input.quantity as f64 * 10.0;

        Outcome::Next(ProcessedOrder {
            order_id,
            item: input.item,
            quantity: input.quantity,
            total,
        })
    }
}

#[derive(Clone)]
struct FormatOutput;

#[async_trait]
impl Transition<ProcessedOrder, OrderConfirmation> for FormatOutput {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: ProcessedOrder,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<OrderConfirmation, Self::Error> {
        let message = format!(
            "Order {} confirmed: {} x {} = ${:.2}",
            input.order_id, input.quantity, input.item, input.total
        );

        Outcome::Next(OrderConfirmation { message })
    }
}

fn rand_id() -> u32 {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hash, Hasher};
    let state = RandomState::new();
    let mut hasher = state.build_hasher();
    std::time::SystemTime::now().hash(&mut hasher);
    (hasher.finish() % 10000) as u32
}

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Testing Patterns Example ===\n");

    let axon = Axon::<OrderRequest, OrderRequest, String, ()>::new("order.pipeline")
        .then(ValidateInput)
        .then(ProcessOrder)
        .then(FormatOutput);

    let mut bus = Bus::new();

    let request = OrderRequest {
        item: "Widget".to_string(),
        quantity: 5,
    };

    println!("Processing order: {:?}", request);

    match axon.execute(request, &(), &mut bus).await {
        Outcome::Next(confirmation) => {
            println!("Success: {}", confirmation.message);
        }
        Outcome::Fault(err) => {
            println!("Error: {}", err);
        }
        other => {
            println!("Unexpected outcome: {:?}", other);
        }
    }

    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------------
    // Unit Tests — Individual Transitions
    // ------------------------------------------------------------------------

    #[tokio::test]
    async fn test_validate_input_success() {
        let transition = ValidateInput;
        let mut bus = Bus::new();
        let input = OrderRequest {
            item: "Widget".to_string(),
            quantity: 3,
        };

        let result = transition.run(input, &(), &mut bus).await;

        match result {
            Outcome::Next(validated) => {
                assert_eq!(validated.item, "Widget");
                assert_eq!(validated.quantity, 3);
            }
            other => panic!("Expected Next, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_validate_input_empty_item() {
        let transition = ValidateInput;
        let mut bus = Bus::new();
        let input = OrderRequest {
            item: "".to_string(),
            quantity: 3,
        };

        let result = transition.run(input, &(), &mut bus).await;

        match result {
            Outcome::Fault(err) => {
                assert_eq!(err, "Item name cannot be empty");
            }
            other => panic!("Expected Fault, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_validate_input_zero_quantity() {
        let transition = ValidateInput;
        let mut bus = Bus::new();
        let input = OrderRequest {
            item: "Widget".to_string(),
            quantity: 0,
        };

        let result = transition.run(input, &(), &mut bus).await;

        match result {
            Outcome::Fault(err) => {
                assert_eq!(err, "Quantity must be greater than 0");
            }
            other => panic!("Expected Fault, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_process_order() {
        let transition = ProcessOrder;
        let mut bus = Bus::new();
        let input = ValidatedOrder {
            item: "Gadget".to_string(),
            quantity: 2,
        };

        let result = transition.run(input, &(), &mut bus).await;

        match result {
            Outcome::Next(processed) => {
                assert!(processed.order_id.starts_with("ORD-"));
                assert_eq!(processed.item, "Gadget");
                assert_eq!(processed.quantity, 2);
                assert_eq!(processed.total, 20.0);
            }
            other => panic!("Expected Next, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_format_output() {
        let transition = FormatOutput;
        let mut bus = Bus::new();
        let input = ProcessedOrder {
            order_id: "ORD-1234".to_string(),
            item: "Widget".to_string(),
            quantity: 5,
            total: 50.0,
        };

        let result = transition.run(input, &(), &mut bus).await;

        match result {
            Outcome::Next(confirmation) => {
                assert!(confirmation.message.contains("ORD-1234"));
                assert!(confirmation.message.contains("5 x Widget"));
                assert!(confirmation.message.contains("$50.00"));
            }
            other => panic!("Expected Next, got: {:?}", other),
        }
    }

    // ------------------------------------------------------------------------
    // Integration Tests — Full Axon Chain
    // ------------------------------------------------------------------------

    #[tokio::test]
    async fn test_full_axon_success() {
        let axon = Axon::<OrderRequest, OrderRequest, String, ()>::new("test.pipeline")
            .then(ValidateInput)
            .then(ProcessOrder)
            .then(FormatOutput);

        let mut bus = Bus::new();
        let request = OrderRequest {
            item: "TestItem".to_string(),
            quantity: 7,
        };

        let result = axon.execute(request, &(), &mut bus).await;

        match result {
            Outcome::Next(confirmation) => {
                assert!(confirmation.message.contains("TestItem"));
                assert!(confirmation.message.contains("7 x"));
            }
            other => panic!("Expected Next, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_full_axon_validation_error() {
        let axon = Axon::<OrderRequest, OrderRequest, String, ()>::new("test.pipeline")
            .then(ValidateInput)
            .then(ProcessOrder)
            .then(FormatOutput);

        let mut bus = Bus::new();
        let request = OrderRequest {
            item: "".to_string(),
            quantity: 5,
        };

        let result = axon.execute(request, &(), &mut bus).await;

        match result {
            Outcome::Fault(err) => {
                assert_eq!(err, "Item name cannot be empty");
            }
            other => panic!("Expected Fault, got: {:?}", other),
        }
    }
}
