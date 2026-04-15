use crate::store::AppStore;
use ranvier_core::prelude::*;
use ranvier_macros::transition;

/// Saga compensation: release reserved inventory.
/// Receives the output of the `reserve_inventory` step as input.
#[transition]
pub async fn release_inventory(
    input: serde_json::Value,
    _res: &(),
    bus: &mut Bus,
) -> Outcome<(), String> {
    let order_id = input["id"].as_u64().unwrap_or(0);
    let items = input["items"].as_array().cloned().unwrap_or_default();

    tracing::warn!(order_id, "COMPENSATION: Releasing reserved inventory");

    if let Ok(store) = bus.get_cloned::<AppStore>() {
        for item in &items {
            let product_id = item["product_id"].as_str().unwrap_or("");
            let quantity = item["quantity"].as_u64().unwrap_or(0) as u32;
            store.release_inventory(product_id, quantity);
        }
    }

    Outcome::Next(())
}
