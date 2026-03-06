use ranvier_core::prelude::*;
use ranvier_macros::transition;
use crate::auth;
use crate::store::AppStore;

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

    // Order ID from path parameter (injected as String in Bus)
    let path = bus.read::<String>().cloned().unwrap_or_default();
    let order_id: u64 = path.parse().unwrap_or(0);

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
