use crate::models::OrderStatus;
use crate::store::AppStore;
use ranvier_core::prelude::*;
use ranvier_macros::transition;

#[transition]
pub async fn process_payment(
    input: serde_json::Value,
    _res: &(),
    bus: &mut Bus,
) -> Outcome<serde_json::Value, String> {
    let order_id = input["id"].as_u64().unwrap_or(0);
    let total = input["total"].as_f64().unwrap_or(0.0);

    // Simulate payment processing
    // Orders > $9999 "fail" for demo purposes
    if total > 9999.0 {
        tracing::warn!(order_id, total, "Payment declined — amount too high");
        if let Ok(store) = bus.get_cloned::<AppStore>() {
            store.update_order_status(order_id, OrderStatus::FailedPayment);
        }
        return Outcome::Fault(format!(
            "Payment declined: amount ${total:.2} exceeds limit"
        ));
    }

    let payment_id = format!("PAY-{}", uuid::Uuid::new_v4());

    // Update order with payment info
    if let Ok(store) = bus.get_cloned::<AppStore>() {
        if let Some(mut order) = store.get_order(order_id) {
            order.status = OrderStatus::Paid;
            order.payment_id = Some(payment_id.clone());
            store.save_order(&order);
        }
    }

    tracing::info!(order_id, %payment_id, amount = total, "Payment processed");

    let mut result = input.clone();
    result["payment_id"] = serde_json::json!(payment_id);
    result["status"] = serde_json::json!("Paid");
    Outcome::Next(result)
}
