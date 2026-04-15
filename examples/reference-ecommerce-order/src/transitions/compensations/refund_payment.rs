use crate::models::OrderStatus;
use crate::store::AppStore;
use ranvier_core::prelude::*;
use ranvier_macros::transition;

/// Saga compensation: refund a processed payment.
/// Receives the output of the `process_payment` step as input.
#[transition]
pub async fn refund_payment(
    input: serde_json::Value,
    _res: &(),
    bus: &mut Bus,
) -> Outcome<(), String> {
    let order_id = input["id"].as_u64().unwrap_or(0);
    let payment_id = input["payment_id"].as_str().unwrap_or("unknown");

    tracing::warn!(order_id, payment_id, "COMPENSATION: Refunding payment");

    if let Ok(store) = bus.get_cloned::<AppStore>() {
        store.record_refund(payment_id);
        store.update_order_status(order_id, OrderStatus::Compensated);
    }

    Outcome::Next(())
}
