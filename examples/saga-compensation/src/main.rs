//! Actual invocation-owned Saga rollback with local effect fixtures.
//! Hook completion is not durable confirmation or proof of a remote refund.
use async_trait::async_trait;
use ranvier_core::prelude::SagaPolicy;
use ranvier_core::{Bus, Outcome, Transition};
use ranvier_runtime::{Axon, ExecutionReport};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Order {
    id: String,
    amount_cents: u64,
    item_count: u32,
}
#[derive(Default, Debug)]
struct Ledger {
    reserved: u32,
    charged: u64,
    shipped: bool,
    events: Vec<&'static str>,
}
#[derive(Clone, Copy)]
enum RefundBehavior {
    Complete,
    Fault,
    #[cfg(test)]
    Panic,
}
struct Resources {
    ledger: Mutex<Ledger>,
    fail_payment: bool,
    fail_shipping: bool,
    refund: RefundBehavior,
}
impl Resources {
    fn new(fail_payment: bool, fail_shipping: bool, refund: RefundBehavior) -> Self {
        Self {
            ledger: Mutex::new(Ledger::default()),
            fail_payment,
            fail_shipping,
            refund,
        }
    }
}
#[derive(Clone)]
struct ReserveInventory;
#[async_trait]
impl Transition<Order, Order> for ReserveInventory {
    type Error = String;
    type Resources = Resources;
    async fn run(&self, order: Order, res: &Resources, _: &mut Bus) -> Outcome<Order, String> {
        if order.id.trim().is_empty() || order.item_count == 0 {
            return Outcome::Fault("Invalid order".into());
        }
        let Ok(mut ledger) = res.ledger.lock() else {
            return Outcome::Fault("Ledger unavailable".into());
        };
        ledger.reserved = order.item_count;
        ledger.events.push("reserve");
        Outcome::Next(order)
    }
}
#[derive(Clone)]
struct ChargePayment;
#[async_trait]
impl Transition<Order, Order> for ChargePayment {
    type Error = String;
    type Resources = Resources;
    async fn run(&self, order: Order, res: &Resources, _: &mut Bus) -> Outcome<Order, String> {
        if res.fail_payment || order.amount_cents > 1_000_000 {
            return Outcome::Fault("Payment declined".into());
        }
        let Ok(mut ledger) = res.ledger.lock() else {
            return Outcome::Fault("Ledger unavailable".into());
        };
        ledger.charged = order.amount_cents;
        ledger.events.push("charge");
        Outcome::Next(order)
    }
}
#[derive(Clone)]
struct ConfirmShipping;
#[async_trait]
impl Transition<Order, Order> for ConfirmShipping {
    type Error = String;
    type Resources = Resources;
    async fn run(&self, order: Order, res: &Resources, _: &mut Bus) -> Outcome<Order, String> {
        if res.fail_shipping {
            return Outcome::Fault("Shipping rejected".into());
        }
        let Ok(mut ledger) = res.ledger.lock() else {
            return Outcome::Fault("Ledger unavailable".into());
        };
        ledger.shipped = true;
        ledger.events.push("ship");
        Outcome::Next(order)
    }
}
#[derive(Clone)]
struct ReleaseInventory;
#[async_trait]
impl Transition<Order, ()> for ReleaseInventory {
    type Error = String;
    type Resources = Resources;
    async fn run(&self, input: Order, res: &Resources, _: &mut Bus) -> Outcome<(), String> {
        let Ok(mut ledger) = res.ledger.lock() else {
            return Outcome::Fault("Ledger unavailable".into());
        };
        if ledger.reserved == 0 {
            return Outcome::Next(());
        }
        if ledger.reserved != input.item_count {
            return Outcome::Fault("Reservation mismatch".into());
        }
        ledger.events.push("release");
        ledger.reserved = 0;
        Outcome::Next(())
    }
}
#[derive(Clone)]
struct RefundPayment;
#[async_trait]
impl Transition<Order, ()> for RefundPayment {
    type Error = String;
    type Resources = Resources;
    async fn run(&self, input: Order, res: &Resources, _: &mut Bus) -> Outcome<(), String> {
        // Record the attempt before injecting failure, without poisoning the ledger lock.
        {
            let Ok(mut ledger) = res.ledger.lock() else {
                return Outcome::Fault("Ledger unavailable".into());
            };
            if !ledger.events.contains(&"charge") {
                return Outcome::Next(());
            }
            if ledger.charged != input.amount_cents {
                return Outcome::Fault("Payment mismatch".into());
            }
            ledger.events.push("refund");
        }
        match res.refund {
            RefundBehavior::Fault => return Outcome::Fault("Refund incomplete".into()),
            #[cfg(test)]
            RefundBehavior::Panic => panic!("explicit test-only rollback panic"),
            RefundBehavior::Complete => {}
        }
        let Ok(mut ledger) = res.ledger.lock() else {
            return Outcome::Fault("Ledger unavailable".into());
        };
        ledger.charged = 0;
        Outcome::Next(())
    }
}
fn pipeline() -> Axon<Order, Order, String, Resources> {
    Axon::new("OrderSaga")
        .with_saga_policy(SagaPolicy::Enabled)
        .then_compensated(ReserveInventory, ReleaseInventory)
        .then_compensated(ChargePayment, RefundPayment)
        .then(ConfirmShipping)
}
fn local_outcome(
    report: ExecutionReport<Outcome<Order, String>>,
) -> anyhow::Result<Outcome<Order, String>> {
    if report
        .cleanup()
        .is_some_and(|cleanup| !cleanup.is_complete())
    {
        anyhow::bail!("Reconciliation required; do not retry automatically");
    }
    match report {
        ExecutionReport::Unpersisted { result, .. } => Ok(result),
        _ => anyhow::bail!("Reconciliation required; do not retry automatically"),
    }
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let saga = pipeline();
    if saga.maybe_export_and_exit()? {
        return Ok(());
    }
    for (items, payment_failure, shipping_failure) in [
        (3, false, false),
        (3, true, false),
        (0, false, false),
        (3, false, true),
    ] {
        let resources = Resources::new(payment_failure, shipping_failure, RefundBehavior::Complete);
        let mut bus = Bus::new();
        let report = saga
            .execute(
                Order {
                    id: "local-order".into(),
                    amount_cents: 14999,
                    item_count: items,
                },
                &resources,
                &mut bus,
            )
            .await;
        match local_outcome(report)? {
            Outcome::Next(_) => println!("Local order completed; no durable confirmation implied"),
            Outcome::Fault(_) => println!("Local order rejected; observed hooks completed"),
            _ => anyhow::bail!("Unexpected control flow"),
        }
    }
    // Demonstrate a returned rollback Fault without claiming compensation succeeded.
    let resources = Resources::new(false, true, RefundBehavior::Fault);
    let mut bus = Bus::new();
    let report = saga
        .execute(
            Order {
                id: "incomplete-refund".into(),
                amount_cents: 14999,
                item_count: 3,
            },
            &resources,
            &mut bus,
        )
        .await;
    if local_outcome(report).is_ok() {
        anyhow::bail!("Incomplete refund was acknowledged");
    }
    println!("Incomplete refund requires reconciliation, without automatic retry");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ranvier_runtime::SagaRollbackOutcome;

    #[tokio::test]
    async fn actual_completed_steps_are_compensated_in_reverse_order() {
        for (
            items,
            fail_payment,
            fail_shipping,
            behavior,
            expected_events,
            reserved,
            charged,
            shipped,
            expected_cleanup,
        ) in [
            (
                3,
                false,
                false,
                RefundBehavior::Complete,
                vec!["reserve", "charge", "ship"],
                3,
                500,
                true,
                None,
            ),
            (
                0,
                false,
                false,
                RefundBehavior::Complete,
                vec![],
                0,
                0,
                false,
                Some(true),
            ),
            (
                3,
                true,
                false,
                RefundBehavior::Complete,
                vec!["reserve", "release"],
                0,
                0,
                false,
                Some(true),
            ),
            (
                3,
                false,
                true,
                RefundBehavior::Complete,
                vec!["reserve", "charge", "refund", "release"],
                0,
                0,
                false,
                Some(true),
            ),
            (
                3,
                false,
                true,
                RefundBehavior::Fault,
                vec!["reserve", "charge", "refund", "release"],
                0,
                500,
                false,
                Some(false),
            ),
            (
                3,
                false,
                true,
                RefundBehavior::Panic,
                vec!["reserve", "charge", "refund", "release"],
                0,
                500,
                false,
                Some(false),
            ),
        ] {
            let resources = Resources::new(fail_payment, fail_shipping, behavior);
            let mut bus = Bus::new();
            let report = pipeline()
                .execute(
                    Order {
                        id: "fixture".into(),
                        amount_cents: 500,
                        item_count: items,
                    },
                    &resources,
                    &mut bus,
                )
                .await;
            assert_eq!(
                report.cleanup().map(|cleanup| cleanup.is_complete()),
                expected_cleanup
            );
            if matches!(behavior, RefundBehavior::Panic) {
                assert_eq!(
                    report.cleanup().unwrap().steps[0].outcome,
                    SagaRollbackOutcome::Panicked
                );
            }
            if matches!(behavior, RefundBehavior::Fault) {
                assert_eq!(
                    report.cleanup().unwrap().steps[0].outcome,
                    SagaRollbackOutcome::Fault
                );
            }
            let outcome = local_outcome(report);
            if expected_cleanup == Some(false) {
                assert_eq!(
                    outcome.unwrap_err().to_string(),
                    "Reconciliation required; do not retry automatically"
                );
            } else if shipped {
                assert!(matches!(outcome.unwrap(), Outcome::Next(_)));
            } else {
                assert!(matches!(outcome.unwrap(), Outcome::Fault(_)));
            }
            let ledger = resources.ledger.lock().unwrap();
            assert_eq!(ledger.events, expected_events);
            assert_eq!(
                (ledger.reserved, ledger.charged, ledger.shipped),
                (reserved, charged, shipped)
            );
        }
    }

    #[test]
    fn actual_main_runs_success_failure_and_incomplete_cleanup_scenarios() {
        main().unwrap();
    }
}
