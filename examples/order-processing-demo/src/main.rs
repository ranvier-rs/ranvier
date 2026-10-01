//! # Order Processing Pipeline
//!
//! Complete order fulfillment flow demonstrating validation, inventory, payment, and shipping in a multi-step Axon chain.
//!
//! ## Run
//! ```bash
//! cargo run -p order-processing-demo
//! ```
//!
//! ## Key Concepts
//! - Multi-step Axon chains with `.then()` composition
//! - Synapse-based external service integrations
//! - Branch outcomes for payment/inventory failures

mod domain;
mod nodes;
mod synapses;

use crate::domain::{OrderRequest, OrderResources, Product};
use crate::nodes::{
    ProcessPayment, RefundPayment, ReleaseInventory, RequireNoDispatch, ReserveInventory,
    ShipOrder, ValidateOrder, ValidationNoEffect,
};
use crate::synapses::{InventorySynapse, PaymentSynapse, ShippingSynapse};
use anyhow::Result;
use ranvier_core::prelude::*;
use ranvier_runtime::Axon;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// Explicit local-demo policy: neither persisted nor incomplete cleanup results
// may be converted into ordinary domain success or automatic retry permission.
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

#[tokio::main]
async fn main() -> Result<()> {
    // 1) Setup data and dependencies
    let products = HashMap::from([
        (
            "p1".to_string(),
            Product {
                id: "p1".into(),
                name: "Laptop".into(),
                price: 1200,
                stock: 5,
            },
        ),
        (
            "p2".to_string(),
            Product {
                id: "p2".into(),
                name: "Mouse".into(),
                price: 50,
                stock: 2,
            },
        ),
    ]);
    let inventory_db = Arc::new(Mutex::new(products));
    let resources = OrderResources {
        inventory: InventorySynapse::new(inventory_db),
        payment: PaymentSynapse::default(),
        shipping: ShippingSynapse::default(),
    };

    // 2) Build Axon execution chain
    let axon = Axon::<OrderRequest, OrderRequest, String, OrderResources>::new("OrderProcessing")
        .with_saga_policy(SagaPolicy::Enabled)
        .then_compensated(ValidateOrder, ValidationNoEffect)
        .then_compensated(ReserveInventory, ReleaseInventory)
        .then_compensated(ProcessPayment, RefundPayment)
        .then_compensated(ShipOrder, RequireNoDispatch);

    // 3) Schematic mode for CLI integration
    if axon.maybe_export_and_exit()? {
        return Ok(());
    }

    println!("\n=== Order Processing Demo ===\n");

    let success_case = OrderRequest {
        order_id: "ORD-123".into(),
        items: vec!["p2".into()],
        total_amount: 50,
    };
    run_order_flow("Success Case", success_case, &axon, &resources).await?;

    let payment_declined_case = OrderRequest {
        order_id: "ORD-124".into(),
        items: vec!["p1".into()],
        total_amount: 1200,
    };
    run_order_flow(
        "Payment Declined Case",
        payment_declined_case,
        &axon,
        &resources,
    )
    .await?;

    let out_of_stock_case = OrderRequest {
        order_id: "ORD-125".into(),
        items: vec!["p2".into(), "p2".into(), "p2".into()],
        total_amount: 150,
    };
    run_order_flow("Out Of Stock Case", out_of_stock_case, &axon, &resources).await?;

    Ok(())
}

async fn run_order_flow(
    title: &str,
    request: OrderRequest,
    axon: &Axon<OrderRequest, String, String, OrderResources>,
    resources: &OrderResources,
) -> Result<()> {
    println!("--- {} ---", title);
    println!("Incoming Request: {:?}", request);
    let mut bus = Bus::new();

    match local_demo_outcome(axon.execute(request, resources, &mut bus).await)? {
        Outcome::Next(tracking) => println!(
            "\n\x1b[32m[SUCCESS] Order Completed! Tracking: {}\x1b[0m",
            tracking
        ),
        Outcome::Branch(reason, _) => println!("\n\x1b[31m[FAILED] Branch: {}\x1b[0m", reason),
        Outcome::Fault(e) => println!("\n\x1b[31m[FAULT] Error: {}\x1b[0m", e),
        Outcome::Jump(id, _) => println!("\n\x1b[33m[JUMP] {}\x1b[0m", id),
        Outcome::Emit(event, _) => println!("\n\x1b[34m[EMIT] {}\x1b[0m", event),
    };

    println!();
    Ok(())
}

#[cfg(test)]
mod execution_contract_tests {
    use super::*;
    use ranvier_runtime::{
        CompletionConfirmation, EffectBoundary, ExecutionReport, PersistenceOperation,
        SagaRollbackOutcome, SagaRollbackReport, SagaRollbackStep,
    };

    #[test]
    fn declared_demo_scenarios_run_through_the_report_policy() {
        main().unwrap();
    }

    #[test]
    fn local_report_policy_preserves_faults_and_rejects_uncertain_or_incomplete_execution() {
        let next = || Outcome::<u32, String>::Next(7);
        let reports = [
            ExecutionReport::NotRequested {
                result: next(),
                cleanup: None,
            },
            ExecutionReport::Recorded {
                result: next(),
                completion: CompletionConfirmation::Confirmed,
                cleanup: None,
            },
            ExecutionReport::Recorded {
                result: next(),
                completion: CompletionConfirmation::NotRequested,
                cleanup: None,
            },
            ExecutionReport::Unconfirmed {
                result: next(),
                operation: PersistenceOperation::Result,
                effect: EffectBoundary::MayHaveRun,
                cleanup: None,
            },
            ExecutionReport::BlockedBeforeEffect {
                operation: PersistenceOperation::Intent,
                effect: EffectBoundary::NotInvoked,
                cleanup: None,
            },
            ExecutionReport::BlockedBeforeEffect {
                operation: PersistenceOperation::Intent,
                effect: EffectBoundary::MayHaveRun,
                cleanup: None,
            },
        ];
        for report in reports {
            assert_eq!(
                local_demo_outcome(report).unwrap_err().to_string(),
                "Execution requires reconciliation before retry"
            );
        }
        for (finished, outcome) in [
            (false, SagaRollbackOutcome::Completed),
            (true, SagaRollbackOutcome::Fault),
            (true, SagaRollbackOutcome::InProgress),
        ] {
            let report = ExecutionReport::Unpersisted {
                result: next(),
                cleanup: Some(SagaRollbackReport {
                    trace_id: "private-trace".into(),
                    finished,
                    steps: vec![SagaRollbackStep {
                        node_id: "private-node".into(),
                        outcome,
                    }],
                }),
            };
            assert_eq!(
                local_demo_outcome(report).unwrap_err().to_string(),
                "Execution requires reconciliation before retry"
            );
        }
        assert!(matches!(
            local_demo_outcome(ExecutionReport::Unpersisted {
                result: next(),
                cleanup: None
            })
            .unwrap(),
            Outcome::Next(7)
        ));
        assert!(
            matches!(local_demo_outcome(ExecutionReport::Unpersisted { result: Outcome::<u32, String>::Fault("expected-domain-failure".into()), cleanup: None }).unwrap(), Outcome::Fault(error) if error == "expected-domain-failure")
        );
    }
}
