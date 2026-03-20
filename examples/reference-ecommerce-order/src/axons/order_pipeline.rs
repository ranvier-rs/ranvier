use ranvier_core::saga::SagaPolicy;
use ranvier_runtime::Axon;

use crate::models::CreateOrderRequest;
use crate::transitions::{
    create_order::create_order,
    process_payment::process_payment,
    reserve_inventory::reserve_inventory,
    schedule_shipping::schedule_shipping,
    compensations::{
        refund_payment::refund_payment,
        release_inventory::release_inventory,
    },
};

/// Build the 4-stage order pipeline with Saga compensation.
///
/// Uses `Axon::typed()` so the pipeline accepts `CreateOrderRequest` directly
/// from `post_typed()` — no manual JSON parsing in the first transition.
///
/// Flow: CreateOrder → ProcessPayment → ReserveInventory → ScheduleShipping
///
/// Compensation (LIFO):
///   - ProcessPayment failure: no compensation needed (order stays Pending)
///   - ReserveInventory failure: RefundPayment (LIFO)
///   - ScheduleShipping failure: ReleaseInventory → RefundPayment (LIFO)
pub fn order_pipeline_circuit() -> Axon<CreateOrderRequest, serde_json::Value, String> {
    Axon::typed::<CreateOrderRequest, String>("order-pipeline")
        .with_saga_policy(SagaPolicy::Enabled)
        .then(create_order)
        .then_compensated(process_payment, refund_payment)
        .then_compensated(reserve_inventory, release_inventory)
        .then(schedule_shipping)
}
