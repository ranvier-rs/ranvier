use crate::routes::{HttpMethod, RouteError, RouteRequest, RouteResponse};
use ranvier_core::prelude::*;

/// Handler for /users/:id/*
#[allow(dead_code)]
pub async fn route_user_by_id(
    req: RouteRequest,
    user_id: &str,
    path: &str,
) -> anyhow::Result<Outcome<RouteResponse, RouteError>> {
    let path = path.trim_start_matches('/');

    match (req.method, path) {
        (HttpMethod::GET, "") | (HttpMethod::GET, "/") => Ok(Outcome::Next(RouteResponse {
            status: 200,
            body: format!("Details for User ID: {}", user_id),
        })),
        (HttpMethod::GET, "posts") => Ok(Outcome::Next(RouteResponse {
            status: 200,
            body: serde_json::to_string(&vec![
                format!("Post 1 by user {}", user_id),
                format!("Post 2 by user {}", user_id),
            ])
            .unwrap_or_default(),
        })),
        _ => Ok(Outcome::Fault(RouteError::NotFound(format!(
            "/api/v1/users/{}/{}",
            user_id, path
        )))),
    }
}
