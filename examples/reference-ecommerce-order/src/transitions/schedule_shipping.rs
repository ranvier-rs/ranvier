use ranvier_core::prelude::*;
use ranvier_macros::transition;
use crate::models::OrderStatus;
use crate::store::AppStore;

#[transition]
pub async fn schedule_shipping(
    input: serde_json::Value,
    _res: &(),
    bus: &mut Bus,
) -> Outcome<serde_json::Value, String> {
    let order_id = input["id"].as_u64().unwrap_or(0);
    let customer_id = input["customer_id"].as_str().unwrap_or("");

    // Simulate shipping: "blocked" customer triggers failure for demo
    if customer_id == "blocked-customer" {
        tracing::warn!(order_id, customer_id, "Shipping unavailable for customer");
        if let Some(store) = bus.read::<AppStore>() {
            store.update_order_status(order_id, OrderStatus::FailedShipping);
        }
        return Outcome::Fault(format!(
            "Shipping unavailable for customer: {customer_id}"
        ));
    }

    let shipping_id = format!("SHIP-{}", uuid::Uuid::new_v4());

    // Update order status
    if let Some(store) = bus.read::<AppStore>() {
        if let Some(mut order) = store.get_order(order_id) {
            order.status = OrderStatus::Shipped;
            order.shipping_id = Some(shipping_id.clone());
            store.save_order(&order);
        }
    }

    tracing::info!(order_id, %shipping_id, "Shipping scheduled");

    let mut result = input.clone();
    result["shipping_id"] = serde_json::json!(shipping_id);
    result["status"] = serde_json::json!("Shipped");
    Outcome::Next(result)
}
