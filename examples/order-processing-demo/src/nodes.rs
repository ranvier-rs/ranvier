use crate::domain::{OrderRequest, OrderResources, ValidatedOrder};
use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_core::synapse::Synapse;

#[derive(Clone)]
pub struct ValidateOrder;

// Validation only reads the catalog; no external effect needs reversal.
#[derive(Clone)]
pub struct ValidationNoEffect;
#[async_trait]
impl Transition<OrderRequest, ()> for ValidationNoEffect {
    type Error = String;
    type Resources = OrderResources;
    async fn run(&self, _: OrderRequest, _: &OrderResources, _: &mut Bus) -> Outcome<(), String> {
        Outcome::Next(())
    }
}

#[async_trait]
impl Transition<OrderRequest, ValidatedOrder> for ValidateOrder {
    type Error = String;
    type Resources = OrderResources;

    async fn run(
        &self,
        request: OrderRequest,
        resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<ValidatedOrder, Self::Error> {
        if request.order_id.trim().is_empty() || request.order_id.len() > 128 {
            return Outcome::Fault("Invalid order identity".into());
        }
        println!(
            "\x1b[1m[Node]\x1b[0m Validating Order #{}...",
            request.order_id
        );

        if request.total_amount == 0 {
            return Outcome::Fault("Invalid amount".into());
        }
        if request.items.is_empty() {
            return Outcome::Branch("empty_cart".into(), None);
        }

        match resources.inventory.quote(&request.items) {
            Ok((catalog_prices, payable_amount)) if payable_amount == request.total_amount => {
                Outcome::Next(ValidatedOrder {
                    reservation_owner: uuid::Uuid::new_v4().to_string(),
                    request,
                    catalog_prices,
                    payable_amount,
                })
            }
            Ok(_) => Outcome::Fault("Catalog amount mismatch".into()),
            Err(error) => Outcome::Fault(error),
        }
    }
}

#[derive(Clone)]
pub struct ReserveInventory;

#[async_trait]
impl Transition<ValidatedOrder, ValidatedOrder> for ReserveInventory {
    type Error = String;
    type Resources = OrderResources;

    async fn run(
        &self,
        request: ValidatedOrder,
        resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<ValidatedOrder, Self::Error> {
        println!("\x1b[1m[Node]\x1b[0m Reserving Inventory...");

        match resources.inventory.call(request.clone()).await {
            Ok(true) => Outcome::Next(request),
            Ok(false) => Outcome::Branch("out_of_stock".into(), None),
            Err(e) => Outcome::Fault(e),
        }
    }
}

#[derive(Clone)]
pub struct ProcessPayment;

#[async_trait]
impl Transition<ValidatedOrder, ValidatedOrder> for ProcessPayment {
    type Error = String;
    type Resources = OrderResources;

    async fn run(
        &self,
        request: ValidatedOrder,
        resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<ValidatedOrder, Self::Error> {
        println!("\x1b[1m[Node]\x1b[0m Processing Payment...");

        if let Err(error) = resources.inventory.begin_payment(&request) {
            return Outcome::Fault(error);
        }

        match resources.payment.call(request.clone()).await {
            Ok(true) => match resources.inventory.confirm_payment(&request, true) {
                Ok(()) => Outcome::Next(request),
                Err(error) => Outcome::Fault(error),
            },
            Ok(false) => {
                if let Err(error) = resources
                    .inventory
                    .confirm_payment(&request, false)
                    .and_then(|()| resources.inventory.release(&request))
                {
                    return Outcome::Fault(error);
                }
                Outcome::Branch("payment_declined".into(), None)
            }
            Err(e) => Outcome::Fault(e),
        }
    }
}

#[derive(Clone)]
pub struct RefundPayment;
#[async_trait]
impl Transition<ValidatedOrder, ()> for RefundPayment {
    type Error = String;
    type Resources = OrderResources;
    async fn run(
        &self,
        order: ValidatedOrder,
        resources: &OrderResources,
        _: &mut Bus,
    ) -> Outcome<(), String> {
        match resources
            .payment
            .refund(&order)
            .and_then(|refunded| resources.inventory.settle_refund(&order, refunded))
        {
            Ok(()) => Outcome::Next(()),
            Err(error) => Outcome::Fault(error),
        }
    }
}

#[derive(Clone)]
pub struct ReleaseInventory;

#[async_trait]
impl Transition<ValidatedOrder, ()> for ReleaseInventory {
    type Error = String;
    type Resources = OrderResources;
    async fn run(
        &self,
        order: ValidatedOrder,
        resources: &OrderResources,
        _: &mut Bus,
    ) -> Outcome<(), String> {
        match resources.inventory.release(&order) {
            Ok(()) => Outcome::Next(()),
            Err(error) => Outcome::Fault(error),
        }
    }
}

#[derive(Clone)]
pub struct ShipOrder;

#[async_trait]
impl Transition<ValidatedOrder, String> for ShipOrder {
    type Error = String;
    type Resources = OrderResources;

    fn description(&self) -> Option<String> {
        Some("Egress step that dispatches the order and returns tracking id".to_string())
    }

    async fn run(
        &self,
        request: ValidatedOrder,
        resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        println!("\x1b[1m[Node]\x1b[0m Shipping Order...");

        if let Err(error) = resources.inventory.authorize_shipping(&request) {
            return Outcome::Fault(error);
        }
        let permit = match resources.payment.begin_dispatch(&request) {
            Ok(permit) => permit,
            Err(error) => return Outcome::Fault(error),
        };
        let result = resources.shipping.call(permit).await;
        if let Err(error) = resources.payment.finish_dispatch(&request, &result) {
            return Outcome::Fault(error);
        }
        match result {
            Ok(tracking) => match resources
                .payment
                .confirm_tracking(&request, &tracking)
                .and_then(|()| resources.shipping.confirm_tracking(&request, &tracking))
                .and_then(|()| resources.inventory.fulfill(&request))
            {
                Ok(()) => Outcome::Next(tracking),
                Err(error) => Outcome::Fault(error),
            },
            Err(_) => Outcome::Fault("Dispatch rejected before effect".into()),
        }
    }
}

#[derive(Clone)]
pub struct RequireNoDispatch;
#[async_trait]
impl Transition<ValidatedOrder, ()> for RequireNoDispatch {
    type Error = String;
    type Resources = OrderResources;
    async fn run(
        &self,
        order: ValidatedOrder,
        resources: &OrderResources,
        _: &mut Bus,
    ) -> Outcome<(), String> {
        match resources.payment.require_no_dispatch(&order) {
            Ok(()) => Outcome::Next(()),
            Err(error) => Outcome::Fault(error),
        }
    }
}

#[cfg(test)]
mod price_tests {
    use super::*;
    use crate::domain::Product;
    use crate::synapses::{InventorySynapse, PaymentSynapse, ShippingSynapse};
    use ranvier_runtime::Axon;
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    fn resources(price: u32) -> OrderResources {
        OrderResources {
            inventory: InventorySynapse::new(Arc::new(Mutex::new(HashMap::from([(
                "p".into(),
                Product {
                    id: "p".into(),
                    name: "fixture".into(),
                    price,
                    stock: 5,
                },
            )])))),
            payment: PaymentSynapse::default(),
            shipping: ShippingSynapse::default(),
        }
    }

    fn request(items: Vec<String>, total_amount: u32) -> OrderRequest {
        OrderRequest {
            order_id: "fixture".into(),
            items,
            total_amount,
        }
    }

    #[tokio::test]
    async fn invalid_quotes_have_no_stock_or_payment_effects() {
        let mut cases = vec![
            (10, request(vec!["p".into()], 1)),
            (10, request(vec!["missing".into()], 10)),
            (u32::MAX, request(vec!["p".into(), "p".into()], u32::MAX)),
            (0, request(vec!["p".into()], 10)),
            (1, request(vec!["p".into(); 1001], 1001)),
            (10, request(vec![], 10)),
        ];
        let mut blank_identity = request(vec!["p".into()], 10);
        blank_identity.order_id = " ".into();
        cases.push((10, blank_identity));
        let axon = Axon::<OrderRequest, OrderRequest, String, OrderResources>::new("price-test")
            .then(ValidateOrder)
            .then(ReserveInventory)
            .then(ProcessPayment);
        for (price, req) in cases {
            let res = resources(price);
            let outcome =
                crate::local_demo_outcome(axon.execute(req, &res, &mut Bus::new()).await).unwrap();
            assert!(matches!(outcome, Outcome::Fault(_) | Outcome::Branch(_, _)));
            assert_eq!(res.inventory.inventory.lock().unwrap()["p"].stock, 5);
            assert!(res.payment.attempted_amounts.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn duplicate_items_charge_exact_catalog_sum() {
        let res = resources(10);
        let axon = Axon::<OrderRequest, OrderRequest, String, OrderResources>::new("price-test")
            .then(ValidateOrder)
            .then(ReserveInventory)
            .then(ProcessPayment);
        let outcome = crate::local_demo_outcome(
            axon.execute(
                request(vec!["p".into(), "p".into()], 20),
                &res,
                &mut Bus::new(),
            )
            .await,
        )
        .unwrap();
        assert!(matches!(
            outcome,
            Outcome::Next(ValidatedOrder {
                payable_amount: 20,
                ..
            })
        ));
        assert_eq!(*res.payment.attempted_amounts.lock().unwrap(), vec![20]);
        assert_eq!(res.inventory.inventory.lock().unwrap()["p"].stock, 3);
    }

    #[tokio::test]
    async fn changed_line_prices_reject_even_when_total_is_unchanged() {
        let res = resources(10);
        res.inventory.inventory.lock().unwrap().insert(
            "q".into(),
            Product {
                id: "q".into(),
                name: "other".into(),
                price: 20,
                stock: 5,
            },
        );
        let validated = match ValidateOrder
            .run(
                request(vec!["p".into(), "q".into()], 30),
                &res,
                &mut Bus::new(),
            )
            .await
        {
            Outcome::Next(value) => value,
            other => panic!("unexpected {other:?}"),
        };
        {
            let mut db = res.inventory.inventory.lock().unwrap();
            db.get_mut("p").unwrap().price = 11;
            db.get_mut("q").unwrap().price = 19;
        }
        assert!(matches!(
            ReserveInventory.run(validated, &res, &mut Bus::new()).await,
            Outcome::Fault(_)
        ));
        let db = res.inventory.inventory.lock().unwrap();
        assert_eq!((db["p"].stock, db["q"].stock), (5, 5));
        assert!(res.payment.attempted_amounts.lock().unwrap().is_empty());
    }

    #[derive(Clone)]
    struct RejectDownstream;
    #[async_trait]
    impl Transition<ValidatedOrder, String> for RejectDownstream {
        type Error = String;
        type Resources = OrderResources;
        async fn run(
            &self,
            _: ValidatedOrder,
            _: &OrderResources,
            _: &mut Bus,
        ) -> Outcome<String, String> {
            Outcome::Fault("Injected downstream failure".into())
        }
    }

    #[derive(Clone)]
    struct FailureNoEffect;
    #[async_trait]
    impl Transition<ValidatedOrder, ()> for FailureNoEffect {
        type Error = String;
        type Resources = OrderResources;
        async fn run(
            &self,
            _: ValidatedOrder,
            _: &OrderResources,
            _: &mut Bus,
        ) -> Outcome<(), String> {
            Outcome::Next(())
        }
    }
    #[tokio::test]
    async fn decline_restores_stock_and_fault_before_payment_rolls_back() {
        let res = resources(1200);
        let axon = Axon::<OrderRequest, OrderRequest, String, OrderResources>::new("decline")
            .with_saga_policy(SagaPolicy::Enabled)
            .then_compensated(ValidateOrder, ValidationNoEffect)
            .then_compensated(ReserveInventory, ReleaseInventory)
            .then(ProcessPayment);
        let outcome = crate::local_demo_outcome(
            axon.execute(request(vec!["p".into()], 1200), &res, &mut Bus::new())
                .await,
        )
        .unwrap();
        assert!(matches!(outcome, Outcome::Branch(ref reason, _) if reason == "payment_declined"));
        assert_eq!(res.inventory.inventory.lock().unwrap()["p"].stock, 5);
        assert_eq!(*res.payment.attempted_amounts.lock().unwrap(), vec![1200]);

        let res = resources(10);
        let axon =
            Axon::<OrderRequest, OrderRequest, String, OrderResources>::new("before-payment")
                .with_saga_policy(SagaPolicy::Enabled)
                .then_compensated(ValidateOrder, ValidationNoEffect)
                .then_compensated(ReserveInventory, ReleaseInventory)
                .then_compensated(RejectDownstream, FailureNoEffect);
        let report = axon
            .execute(request(vec!["p".into()], 10), &res, &mut Bus::new())
            .await;
        assert!(
            report
                .cleanup()
                .is_some_and(|cleanup| cleanup.is_complete())
        );
        assert!(matches!(
            crate::local_demo_outcome(report).unwrap(),
            Outcome::Fault(_)
        ));
        assert_eq!(res.inventory.inventory.lock().unwrap()["p"].stock, 5);
        assert!(res.payment.attempted_amounts.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn failure_after_charge_requires_reconciliation_and_preserves_stock() {
        let res = resources(10);
        let axon = Axon::<OrderRequest, OrderRequest, String, OrderResources>::new("after-payment")
            .with_saga_policy(SagaPolicy::Enabled)
            .then_compensated(ValidateOrder, ValidationNoEffect)
            .then_compensated(ReserveInventory, ReleaseInventory)
            .then(ProcessPayment)
            .then_compensated(RejectDownstream, FailureNoEffect);
        let report = axon
            .execute(request(vec!["p".into()], 10), &res, &mut Bus::new())
            .await;
        assert!(
            report
                .cleanup()
                .is_some_and(|cleanup| !cleanup.is_complete())
        );
        assert!(crate::local_demo_outcome(report).is_err());
        assert_eq!(res.inventory.inventory.lock().unwrap()["p"].stock, 4);
        assert_eq!(*res.payment.attempted_amounts.lock().unwrap(), vec![10]);
    }

    #[tokio::test]
    async fn actual_refund_hook_restores_only_after_confirmed_refund() {
        use crate::synapses::RefundBehavior;
        for (behavior, complete) in [
            (RefundBehavior::Complete, true),
            (RefundBehavior::Fault, false),
            (RefundBehavior::Uncertain, false),
            (RefundBehavior::Panic, false),
        ] {
            let mut res = resources(10);
            res.payment.refund_behavior = behavior;
            let axon =
                Axon::<OrderRequest, OrderRequest, String, OrderResources>::new("refund-test")
                    .with_saga_policy(SagaPolicy::Enabled)
                    .then_compensated(ValidateOrder, ValidationNoEffect)
                    .then_compensated(ReserveInventory, ReleaseInventory)
                    .then_compensated(ProcessPayment, RefundPayment)
                    .then_compensated(RejectDownstream, FailureNoEffect);
            let report = axon
                .execute(request(vec!["p".into()], 10), &res, &mut Bus::new())
                .await;
            assert_eq!(report.cleanup().unwrap().is_complete(), complete);
            if complete {
                assert!(matches!(
                    crate::local_demo_outcome(report).unwrap(),
                    Outcome::Fault(_)
                ));
                assert_eq!(res.inventory.inventory.lock().unwrap()["p"].stock, 5);
                assert_eq!(res.payment.test_balance("fixture"), (0, true));
            } else {
                assert!(crate::local_demo_outcome(report).is_err());
                assert_eq!(res.inventory.inventory.lock().unwrap()["p"].stock, 4);
                assert_eq!(res.payment.test_balance("fixture"), (10, false));
            }
        }
    }

    #[tokio::test]
    async fn complete_dispatch_and_definite_or_uncertain_rejection_have_distinct_recovery() {
        use crate::synapses::ShippingBehavior;
        for behavior in [
            ShippingBehavior::Complete,
            ShippingBehavior::Reject,
            ShippingBehavior::Uncertain,
        ] {
            let mut res = resources(10);
            res.shipping.behavior = behavior;
            let axon =
                Axon::<OrderRequest, OrderRequest, String, OrderResources>::new("dispatch-test")
                    .with_saga_policy(SagaPolicy::Enabled)
                    .then_compensated(ValidateOrder, ValidationNoEffect)
                    .then_compensated(ReserveInventory, ReleaseInventory)
                    .then_compensated(ProcessPayment, RefundPayment)
                    .then_compensated(ShipOrder, RequireNoDispatch);
            let report = axon
                .execute(request(vec!["p".into()], 10), &res, &mut Bus::new())
                .await;
            match behavior {
                ShippingBehavior::Complete => {
                    let outcome = crate::local_demo_outcome(report).unwrap();
                    let tracking = match outcome {
                        Outcome::Next(value) => value,
                        other => panic!("unexpected {other:?}"),
                    };
                    assert_eq!(res.shipping.test_tracking("fixture"), Some(tracking));
                    assert_eq!(res.payment.test_balance("fixture"), (10, false));
                    assert_eq!(res.inventory.inventory.lock().unwrap()["p"].stock, 4);
                }
                ShippingBehavior::Reject => {
                    assert!(report.cleanup().unwrap().is_complete());
                    assert!(matches!(
                        crate::local_demo_outcome(report).unwrap(),
                        Outcome::Fault(_)
                    ));
                    assert_eq!(res.shipping.test_tracking("fixture"), None);
                    assert_eq!(res.payment.test_balance("fixture"), (0, true));
                    assert_eq!(res.inventory.inventory.lock().unwrap()["p"].stock, 5);
                }
                ShippingBehavior::Uncertain => {
                    assert!(!report.cleanup().unwrap().is_complete());
                    assert!(crate::local_demo_outcome(report).is_err());
                    assert!(res.shipping.test_tracking("fixture").is_some()); // Effect happened, response is ambiguous.
                    assert_eq!(res.payment.test_balance("fixture"), (10, false));
                    assert_eq!(res.inventory.inventory.lock().unwrap()["p"].stock, 4);
                }
            }
        }
    }

    async fn paid_order() -> (OrderResources, ValidatedOrder) {
        let res = resources(10);
        let axon = Axon::<OrderRequest, OrderRequest, String, OrderResources>::new("paid")
            .then(ValidateOrder)
            .then(ReserveInventory)
            .then(ProcessPayment);
        let outcome = crate::local_demo_outcome(
            axon.execute(request(vec!["p".into()], 10), &res, &mut Bus::new())
                .await,
        )
        .unwrap();
        match outcome {
            Outcome::Next(order) => (res, order),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn concurrent_dispatch_is_single_and_terminal_order_cannot_be_refunded() {
        let (res, order) = paid_order().await;
        let mut first_bus = Bus::new();
        let mut second_bus = Bus::new();
        let (first, second) = tokio::join!(
            ShipOrder.run(order.clone(), &res, &mut first_bus),
            ShipOrder.run(order.clone(), &res, &mut second_bus)
        );
        assert_eq!(
            usize::from(matches!(first, Outcome::Next(_)))
                + usize::from(matches!(second, Outcome::Next(_))),
            1
        );
        assert!(matches!(
            ShipOrder.run(order.clone(), &res, &mut Bus::new()).await,
            Outcome::Fault(_)
        ));
        assert!(res.payment.refund(&order).is_err());
        assert_eq!(res.payment.test_balance("fixture"), (10, false));
        assert_eq!(res.inventory.inventory.lock().unwrap()["p"].stock, 4);
    }

    #[tokio::test]
    async fn dispatch_rejects_wrong_owner_and_amount_before_effect() {
        let (res, order) = paid_order().await;
        for tamper in 0..4 {
            let mut changed = order.clone();
            match tamper {
                0 => changed.reservation_owner = "stranger".into(),
                1 => changed.payable_amount += 1,
                2 => changed.request.total_amount += 1,
                _ => changed.request.items = vec!["different-cart".into()],
            }
            assert!(matches!(
                ShipOrder.run(changed, &res, &mut Bus::new()).await,
                Outcome::Fault(_)
            ));
        }
        assert!(res.shipping.test_tracking("fixture").is_none());
        assert!(matches!(
            ShipOrder.run(order, &res, &mut Bus::new()).await,
            Outcome::Next(_)
        ));
    }

    #[tokio::test]
    async fn dropped_dispatch_intent_blocks_refund_and_repeat_dispatch() {
        let (res, order) = paid_order().await;
        let mut bus = Bus::new();
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(10),
                ShipOrder.run(order.clone(), &res, &mut bus)
            )
            .await
            .is_err()
        );
        assert!(res.payment.refund(&order).is_err());
        assert!(matches!(
            ShipOrder.run(order, &res, &mut Bus::new()).await,
            Outcome::Fault(_)
        ));
        assert!(res.shipping.test_tracking("fixture").is_none());
        assert_eq!(res.payment.test_balance("fixture"), (10, false));
        assert_eq!(res.inventory.inventory.lock().unwrap()["p"].stock, 4);
    }
}
