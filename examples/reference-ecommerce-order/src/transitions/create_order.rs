use ranvier_core::prelude::*;
use ranvier_macros::transition;
use crate::auth;
use crate::models::{CreateOrderRequest, Order};
use crate::store::AppStore;

#[transition]
pub async fn create_order(
    _input: (),
    _res: &(),
    bus: &mut Bus,
) -> Outcome<serde_json::Value, String> {
    // Extract JWT from Authorization header (injected by HTTP adapter)
    let auth_header = bus
        .read::<Vec<(String, String)>>()
        .and_then(|headers| {
            headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
                .map(|(_, v)| v.clone())
        })
        .unwrap_or_default();

    let token = auth_header
        .strip_prefix("Bearer ")
        .unwrap_or(&auth_header);

    let claims = match auth::verify_token(token) {
        Some(c) => c,
        None => return Outcome::Fault("Unauthorized: invalid or missing JWT".to_string()),
    };

    let body = bus.read::<String>().cloned().unwrap_or_default();
    let request: CreateOrderRequest = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(_) => return Outcome::Fault("Invalid JSON body".to_string()),
    };

    if request.items.is_empty() {
        return Outcome::Fault("Order must contain at least one item".to_string());
    }

    let order = Order::new(claims.tenant_id, request.customer_id, request.items);

    // Store order
    if let Some(store) = bus.read::<AppStore>() {
        store.save_order(&order);
    }

    tracing::info!(order_id = order.id, total = order.total, "Order created");

    Outcome::Next(serde_json::to_value(&order).unwrap())
}
