use ranvier_core::prelude::*;
use ranvier_macros::transition;
use crate::auth;
use crate::store::AppStore;

#[transition]
pub async fn list_orders(
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

    let orders = bus
        .read::<AppStore>()
        .map(|store| store.list_orders(&claims.tenant_id))
        .unwrap_or_default();

    Outcome::Next(serde_json::json!({
        "tenant_id": claims.tenant_id,
        "orders": orders,
        "count": orders.len()
    }))
}
