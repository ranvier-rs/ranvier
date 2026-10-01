use crate::domain::{OrderRequest, OrderResources, ValidatedOrder};
use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_core::synapse::Synapse;

#[derive(Clone)]
pub struct ValidateOrder;

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

        match resources.payment.call(request.payable_amount).await {
            Ok(true) => Outcome::Next(request),
            Ok(false) => Outcome::Branch("payment_declined".into(), None),
            Err(e) => Outcome::Fault(e),
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

        match resources.shipping.call(request.request.order_id).await {
            Ok(tracking) => Outcome::Next(tracking),
            Err(e) => Outcome::Fault(e),
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
            inventory: InventorySynapse {
                inventory: Arc::new(Mutex::new(HashMap::from([(
                    "p".into(),
                    Product {
                        id: "p".into(),
                        name: "fixture".into(),
                        price,
                        stock: 5,
                    },
                )]))),
            },
            payment: PaymentSynapse::default(),
            shipping: ShippingSynapse,
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
}
