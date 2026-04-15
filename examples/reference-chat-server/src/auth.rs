use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Simple JWT-like token store for the chat server demo.
/// In production, use a real JWT library (jsonwebtoken crate).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub user_id: String,
    pub username: String,
}

/// Token store: maps token strings to claims.
pub type TokenStore = Arc<Mutex<HashMap<String, Claims>>>;

pub fn new_token_store() -> TokenStore {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Issue a token for a user (demo: token = "tok_{user_id}").
pub fn issue_token(store: &TokenStore, user_id: &str, username: &str) -> String {
    let token = format!("tok_{}", user_id);
    let claims = Claims {
        user_id: user_id.to_string(),
        username: username.to_string(),
    };
    store.lock().unwrap().insert(token.clone(), claims);
    token
}

/// Verify a token and return claims.
pub fn verify_token(store: &TokenStore, token: &str) -> Option<Claims> {
    store.lock().unwrap().get(token).cloned()
}

/// Extract bearer token from "Authorization: Bearer <token>" header.
pub fn extract_bearer(header_value: &str) -> Option<&str> {
    header_value
        .strip_prefix("Bearer ")
        .or_else(|| header_value.strip_prefix("bearer "))
}
