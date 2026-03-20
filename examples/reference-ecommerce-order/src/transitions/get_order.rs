use ranvier_core::prelude::*;
use ranvier_http::PathParams;
use ranvier_macros::transition;
use crate::auth;
use crate::store::AppStore;

/// Get order by ID — reads `:id` path param from `PathParams` in Bus.
#[transition]
pub async fn get_order(
    _input: (),
    _res: &(),
    bus: &mut Bus,
) -> Outcome<serde_json::Value, String> {
    let auth_header = bus
        .read::<Vec<(String, String)>>()
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
        None => return Outcome::Fault("Unauthorized".to_string()),
    };

    let order_id: u64 = match bus.read::<PathParams>().and_then(|p| p.get("id")) {
        Some(id_str) => match id_str.parse() {
            Ok(id) => id,
            Err(_) => return Outcome::Fault("Invalid order ID".to_string()),
        },
        None => return Outcome::Fault("Missing order ID".to_string()),
    };

    let store = match bus.read::<AppStore>() {
        Some(s) => s.clone(),
        None => return Outcome::Fault("Store unavailable".to_string()),
    };

    match store.get_order(order_id) {
        Some(order) if order.tenant_id == claims.tenant_id => {
            Outcome::Next(serde_json::to_value(&order).unwrap())
        }
        Some(_) => Outcome::Fault("Unauthorized: order belongs to different tenant".to_string()),
        None => Outcome::Fault(format!("Order not found: {order_id}")),
    }
}
