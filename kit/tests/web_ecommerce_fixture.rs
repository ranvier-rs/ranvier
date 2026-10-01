// Exact public ecommerce chain with explicit test-only effect adapters.
#[rustfmt::skip]
mod public_order {
use ranvier_core::{Bus, Outcome};
use ranvier_macros::transition;
use ranvier_runtime::Axon;
use serde::{Serialize, Deserialize};
use std::{collections::HashMap, sync::Arc};

// Trusted ingress supplies identity; client order JSON cannot select a price or role.
#[derive(Clone)]
struct Shopper { subject: String, vip: bool }
#[derive(Clone, Serialize, Deserialize)]
struct OrderRequest { order_id: String, sku: String, quantity: u32 }
#[derive(Clone, Serialize, Deserialize)]
struct OrderContext {
    request: OrderRequest, subject: String, vip: bool,
    subtotal: u64, tax: u64, discount: u64, total: u64,
    payment_id: Option<String>, shipment_id: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum OrderError { Unauthenticated, InvalidOrder, OutOfStock, Arithmetic, Payment, Shipment }
struct Product { unit_cents: u64, available: u32 }
struct OrderResources {
    catalog: HashMap<String, Product>, tax_basis_points: u32,
    effects: Arc<dyn OrderEffects>,
}
// Implementations must bind order_id to subject and total and reconcile uncertain
// effects. No automatic retry or durable completion is implied by this local example.
#[async_trait::async_trait]
trait OrderEffects: Send + Sync {
    async fn charge(&self, order: &OrderContext) -> Result<String, OrderError>;
    async fn ship(&self, order: &OrderContext) -> Result<String, OrderError>;
}

#[transition(resources = OrderResources)]
async fn authenticate_order(request: OrderRequest, bus: &mut Bus) -> Outcome<OrderContext, OrderError> {
    let Some(shopper) = bus.read::<Shopper>().filter(|s| !s.subject.trim().is_empty()) else {
        return Outcome::Fault(OrderError::Unauthenticated);
    };
    Outcome::Next(OrderContext { request, subject: shopper.subject.clone(), vip: shopper.vip,
        subtotal: 0, tax: 0, discount: 0, total: 0, payment_id: None, shipment_id: None })
}
#[transition(resources = OrderResources)]
async fn validate_order(input: OrderContext) -> Outcome<OrderContext, OrderError> {
    if input.request.quantity == 0 || input.request.quantity > 1000
        || input.request.order_id.trim().is_empty() || input.request.order_id.len() > 128
        || input.request.sku.trim().is_empty() || input.request.sku.len() > 128 {
        return Outcome::Fault(OrderError::InvalidOrder);
    }
    Outcome::Next(input)
}
#[transition]
async fn check_inventory(mut input: OrderContext, resources: &OrderResources) -> Outcome<OrderContext, OrderError> {
    let Some(product) = resources.catalog.get(&input.request.sku) else { return Outcome::Fault(OrderError::OutOfStock); };
    if product.available < input.request.quantity { return Outcome::Fault(OrderError::OutOfStock); }
    let Some(subtotal) = product.unit_cents.checked_mul(u64::from(input.request.quantity)) else { return Outcome::Fault(OrderError::Arithmetic); };
    input.subtotal = subtotal;
    Outcome::Next(input)
}
// Monetary units are integer cents. Tax is on the discounted subtotal; fractions
// of a cent round down. These are explicit example policies, not jurisdiction rules.
#[transition(resources = OrderResources)]
async fn apply_discount(mut input: OrderContext) -> Outcome<OrderContext, OrderError> {
    input.discount = if input.vip { input.subtotal / 10 } else { 0 };
    Outcome::Next(input)
}
#[transition]
async fn calculate_tax(mut input: OrderContext, resources: &OrderResources) -> Outcome<OrderContext, OrderError> {
    if resources.tax_basis_points > 10000 { return Outcome::Fault(OrderError::Arithmetic); }
    let Some(net) = input.subtotal.checked_sub(input.discount) else { return Outcome::Fault(OrderError::Arithmetic); };
    let Ok(tax) = u64::try_from(u128::from(net) * u128::from(resources.tax_basis_points) / 10000) else { return Outcome::Fault(OrderError::Arithmetic); };
    let Some(total) = net.checked_add(tax) else { return Outcome::Fault(OrderError::Arithmetic); };
    input.tax = tax;
    input.total = total;
    Outcome::Next(input)
}
#[transition]
async fn charge_payment(mut input: OrderContext, resources: &OrderResources) -> Outcome<OrderContext, OrderError> {
    match resources.effects.charge(&input).await {
        Ok(id) if !id.trim().is_empty() => { input.payment_id = Some(id); Outcome::Next(input) }
        _ => Outcome::Fault(OrderError::Payment),
    }
}
#[transition]
async fn create_shipment(mut input: OrderContext, resources: &OrderResources) -> Outcome<OrderContext, OrderError> {
    if input.payment_id.is_none() { return Outcome::Fault(OrderError::Payment); }
    match resources.effects.ship(&input).await {
        Ok(id) if !id.trim().is_empty() => { input.shipment_id = Some(id); Outcome::Next(input) }
        _ => Outcome::Fault(OrderError::Shipment),
    }
}
fn order_pipeline() -> Axon<OrderRequest, OrderContext, OrderError, OrderResources> {
    Axon::new("order-processing")
        .then(authenticate_order).then(validate_order).then(check_inventory)
        .then(apply_discount).then(calculate_tax).then(charge_payment).then(create_shipment)
}
// Inventory is a read-only snapshot here; production needs atomic reservation.
// Shipment failure can follow a completed charge: reconcile, never blindly rerun.
// Callers must inspect ExecutionReport durability and cleanup before acknowledgement.

use std::sync::atomic::{AtomicUsize, Ordering};
struct FixtureEffects { charged: AtomicUsize, shipped: AtomicUsize, fail_payment: bool, fail_shipment: bool }
#[async_trait::async_trait]
impl OrderEffects for FixtureEffects {
    async fn charge(&self, order: &OrderContext) -> Result<String, OrderError> {
        self.charged.fetch_add(1, Ordering::SeqCst);
        assert!(!order.subject.is_empty());
        if self.fail_payment { Err(OrderError::Payment) } else { Ok("fixture-payment".into()) }
    }
    async fn ship(&self, order: &OrderContext) -> Result<String, OrderError> {
        self.shipped.fetch_add(1, Ordering::SeqCst);
        assert_eq!(order.payment_id.as_deref(), Some("fixture-payment"));
        if self.fail_shipment { Err(OrderError::Shipment) } else { Ok("fixture-shipment".into()) }
    }
}

#[tokio::test]
async fn actual_order_chain_preserves_money_identity_and_effect_boundaries() {
    for (identity, vip, quantity, stock, price, payment_failure, shipment_failure, expected, charges, shipments) in [
        (false, false, 2, 10, 1000, false, false, Some(OrderError::Unauthenticated), 0, 0),
        (true, false, 0, 10, 1000, false, false, Some(OrderError::InvalidOrder), 0, 0),
        (true, false, 2, 1, 1000, false, false, Some(OrderError::OutOfStock), 0, 0),
        (true, false, 2, 10, u64::MAX, false, false, Some(OrderError::Arithmetic), 0, 0),
        (true, false, 2, 10, 1000, true, false, Some(OrderError::Payment), 1, 0),
        (true, false, 2, 10, 1000, false, true, Some(OrderError::Shipment), 1, 1),
        (true, false, 2, 10, 1000, false, false, None, 1, 1),
        (true, true, 2, 10, 1000, false, false, None, 1, 1),
    ] {
        let effects = Arc::new(FixtureEffects { charged: AtomicUsize::new(0), shipped: AtomicUsize::new(0), fail_payment: payment_failure, fail_shipment: shipment_failure });
        let resources = OrderResources { catalog: [("sku".into(), Product { unit_cents: price, available: stock })].into(), tax_basis_points: 1000, effects: effects.clone() };
        let mut bus = Bus::new();
        if identity { bus.insert(Shopper { subject: "alice".into(), vip }); }
        let request = OrderRequest { order_id: "order-1".into(), sku: "sku".into(), quantity };
        let report = order_pipeline().execute(request, &resources, &mut bus).await;
        assert!(!report.cleanup().is_some_and(|cleanup| !cleanup.is_complete()));
        match report {
            ranvier_runtime::ExecutionReport::Unpersisted { result: Outcome::Next(order), .. } => {
                assert!(expected.is_none());
                assert_eq!(order.subject, "alice");
                assert_eq!(order.total, if vip { 1980 } else { 2200 });
                assert_eq!(order.shipment_id.as_deref(), Some("fixture-shipment"));
            }
            ranvier_runtime::ExecutionReport::Unpersisted { result: Outcome::Fault(error), .. } => assert_eq!(Some(error), expected),
            _ => panic!("unexpected order report"),
        }
        assert_eq!(effects.charged.load(Ordering::SeqCst), charges);
        assert_eq!(effects.shipped.load(Ordering::SeqCst), shipments);
    }
}

#[tokio::test]
async fn reviewed_tax_handles_wide_intermediates_and_total_overflow_before_effects() {
    for (price, basis_points, expected) in [
        (u64::MAX, 0, Some(u64::MAX)),
        (u64::MAX / 2, 1000, Some(u64::MAX / 2 + (u64::MAX / 2) / 10)),
        (u64::MAX, 1, None),
        (1000, 10001, None),
    ] {
        let effects = Arc::new(FixtureEffects { charged: AtomicUsize::new(0), shipped: AtomicUsize::new(0), fail_payment: false, fail_shipment: false });
        let resources = OrderResources { catalog: [("sku".into(), Product { unit_cents: price, available: 1 })].into(), tax_basis_points: basis_points, effects: effects.clone() };
        let mut bus = Bus::new();
        bus.insert(Shopper { subject: "alice".into(), vip: false });
        let report = order_pipeline().execute(OrderRequest { order_id: "wide-order".into(), sku: "sku".into(), quantity: 1 }, &resources, &mut bus).await;
        match report {
            ranvier_runtime::ExecutionReport::Unpersisted { result: Outcome::Next(order), .. } => assert_eq!(Some(order.total), expected),
            ranvier_runtime::ExecutionReport::Unpersisted { result: Outcome::Fault(OrderError::Arithmetic), .. } => assert!(expected.is_none()),
            _ => panic!("unexpected arithmetic report"),
        }
        assert_eq!(effects.charged.load(Ordering::SeqCst), usize::from(expected.is_some()));
        assert_eq!(effects.shipped.load(Ordering::SeqCst), usize::from(expected.is_some()));
    }
}
}
