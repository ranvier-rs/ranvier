use crate::routes::{RouteError, RouteRequest, RouteResponse};
use ranvier_core::prelude::*;

pub mod users;

/// V1 API routing transition
pub async fn route_v1(
    req: RouteRequest,
    path: &str,
) -> anyhow::Result<Outcome<RouteResponse, RouteError>> {
    // path is relative to /api/v1 (e.g. "/users")
    let path = path.trim_start_matches('/');

    // Route to /users
    if path == "users" || path.starts_with("users/") {
        return users::route_users(req, &path["users".len()..]).await;
    }

    // API v1 root
    if path.is_empty() || path == "/" {
        return Ok(Outcome::Next(RouteResponse {
            status: 200,
            body: "API v1 Root".to_string(),
        }));
    }

    Ok(Outcome::Fault(RouteError::NotFound(format!(
        "/api/v1/{}",
        path
    ))))
}
