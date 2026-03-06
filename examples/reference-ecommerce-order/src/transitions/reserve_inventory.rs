use ranvier_core::prelude::*;
use ranvier_macros::transition;
use crate::models::OrderStatus;
use crate::store::AppStore;

#[transition]
pub async fn reserve_inventory(
    input: serde_json::Value,
    _res: &(),
    bus: &mut Bus,
) -> Outcome<serde_json::Value, String> {
    let order_id = input["id"].as_u64().unwrap_or(0);
    let items = input["items"].as_array().cloned().unwrap_or_default();

    let store = match bus.read::<AppStore>() {
        Some(s) => s.clone(),
        None => return Outcome::Fault("Store unavailable".to_string()),
    };

    // Try to reserve each item
    let mut reserved: Vec<(String, u32)> = Vec::new();
    for item in &items {
        let product_id = item["product_id"].as_str().unwrap_or("");
        let quantity = item["quantity"].as_u64().unwrap_or(0) as u32;

        if !store.reserve_inventory(product_id, quantity) {
            // Roll back already-reserved items
            for (pid, qty) in &reserved {
                store.release_inventory(pid, *qty);
            }
            tracing::warn!(order_id, product_id, "Insufficient inventory");
            store.update_order_status(order_id, OrderStatus::FailedInventory);
            return Outcome::Fault(format!(
                "Insufficient inventory for product: {product_id}"
            ));
        }
        reserved.push((product_id.to_string(), quantity));
    }

    // Update order status
    store.update_order_status(order_id, OrderStatus::Reserved);

    let reservation_id = format!("RES-{}", uuid::Uuid::new_v4());
    tracing::info!(order_id, %reservation_id, items = reserved.len(), "Inventory reserved");

    let mut result = input.clone();
    result["reservation_id"] = serde_json::json!(reservation_id);
    result["status"] = serde_json::json!("Reserved");
    Outcome::Next(result)
}
