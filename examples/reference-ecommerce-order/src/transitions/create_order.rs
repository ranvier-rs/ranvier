use crate::auth;
use crate::models::{CreateOrderRequest, Order};
use crate::store::AppStore;
use ranvier_core::prelude::*;
use ranvier_macros::transition;

/// Create order transition — receives `CreateOrderRequest` directly via `post_typed()`.
///
/// No manual `serde_json::from_str` needed: the HTTP ingress auto-deserializes
/// the JSON body and passes the typed struct as the saga pipeline input.
///
/// Auth header is read from Bus (injected by `bus_injector`).
#[transition]
pub async fn create_order(
    request: CreateOrderRequest,
    _res: &(),
    bus: &mut Bus,
) -> Outcome<serde_json::Value, String> {
    // Extract JWT from Authorization header (injected into Bus by bus_injector)
    let auth_header = bus
        .get_cloned::<Vec<(String, String)>>()
        .ok()
        .and_then(|headers| {
            headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
                .map(|(_, v)| v.clone())
        })
        .unwrap_or_default();

    let token = auth_header.strip_prefix("Bearer ").unwrap_or(&auth_header);

    let claims = match auth::verify_token(token) {
        Some(c) => c,
        None => return Outcome::Fault("Unauthorized: invalid or missing JWT".to_string()),
    };

    if request.items.is_empty() {
        return Outcome::Fault("Order must contain at least one item".to_string());
    }

    let order = Order::new(claims.tenant_id, request.customer_id, request.items);

    // Store order
    if let Ok(store) = bus.get_cloned::<AppStore>() {
        store.save_order(&order);
    }

    tracing::info!(order_id = order.id, total = order.total, "Order created");

    Outcome::Next(serde_json::to_value(&order).unwrap())
}
