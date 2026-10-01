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
