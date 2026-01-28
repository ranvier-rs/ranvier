use crate::routes::{HttpMethod, RouteError, RouteRequest, RouteResponse};
use ranvier_core::prelude::*;

pub mod by_id;

/// Users routing transition
/// Handles /api/v1/users and /api/v1/users/:id/*
pub async fn route_users(
    req: RouteRequest,
    path: &str,
) -> anyhow::Result<Outcome<RouteResponse, RouteError>> {
    // path comes in relative to /api/v1/users (e.g. "" or "/123" or "/123/posts")
    let path = path.trim_start_matches('/');

    // Check for dynamic segment (User ID)
    if let Some((id, rest)) = extract_segment(path) {
        if id.chars().all(char::is_numeric) {
            return by_id::route_user_by_id(req, id, rest).await;
        }
    }

    // Static routes for /api/v1/users
    match (req.method, path) {
        (HttpMethod::GET, "") | (HttpMethod::GET, "/") => Ok(Outcome::Next(RouteResponse {
            status: 200,
            body: serde_json::to_string(&vec!["User1", "User2"]).unwrap_or_default(),
        })),
        (HttpMethod::POST, "") | (HttpMethod::POST, "/") => Ok(Outcome::Next(RouteResponse {
            status: 201,
            body: "User Created".to_string(),
        })),
        _ => Ok(Outcome::Fault(RouteError::NotFound(format!("/api/v1/users/{}", path)))),
    }
}

/// Helper to extract the first path segment
fn extract_segment(path: &str) -> Option<(&str, &str)> {
    let rest = path.trim_start_matches('/');
    if rest.is_empty() {
        return None;
    }
    let parts = rest.splitn(2, '/').collect::<Vec<_>>();
    if parts.len() == 1 {
        Some((parts[0], ""))
    } else {
        Some((parts[0], parts[1]))
    }
}
