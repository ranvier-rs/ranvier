use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::{Arc, Mutex, OnceLock};

/// Descriptor for a registered HTTP route in the running application.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RouteInfo {
    pub method: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub circuit_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
}

/// Global route registry for Inspector API.
static ROUTE_REGISTRY: OnceLock<Arc<Mutex<Vec<RouteInfo>>>> = OnceLock::new();

fn get_registry() -> Arc<Mutex<Vec<RouteInfo>>> {
    ROUTE_REGISTRY
        .get_or_init(|| Arc::new(Mutex::new(Vec::new())))
        .clone()
}

/// Register a set of routes with the Inspector route registry.
pub fn register_routes(routes: Vec<RouteInfo>) {
    if let Ok(mut registry) = get_registry().lock() {
        registry.extend(routes);
    }
}

/// List all registered routes.
pub fn list_routes() -> Vec<RouteInfo> {
    get_registry()
        .lock()
        .map(|r| r.clone())
        .unwrap_or_default()
}

/// Find a route by method and path.
pub fn find_route(method: &str, path: &str) -> Option<RouteInfo> {
    let routes = list_routes();
    routes
        .into_iter()
        .find(|r| r.method.eq_ignore_ascii_case(method) && r.path == path)
}

/// Clear all registered routes.
pub fn clear_routes() {
    if let Ok(mut registry) = get_registry().lock() {
        registry.clear();
    }
}

/// Request body for `POST /api/v1/routes/schema`.
#[derive(Debug, Deserialize)]
pub struct SchemaLookupRequest {
    pub method: String,
    pub path: String,
}

/// Request body for `POST /api/v1/routes/sample`.
#[derive(Debug, Deserialize)]
pub struct SampleRequest {
    pub method: String,
    pub path: String,
    /// `"empty"` for template, `"random"` for sample
    #[serde(default = "default_mode")]
    pub mode: String,
}

fn default_mode() -> String {
    "empty".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_list_routes() {
        clear_routes();
        register_routes(vec![RouteInfo {
            method: "GET".to_string(),
            path: "/users".to_string(),
            circuit_name: Some("UserList".to_string()),
            input_schema: None,
            output_schema: None,
        }]);

        let routes = list_routes();
        assert!(!routes.is_empty());
        assert_eq!(routes.last().unwrap().path, "/users");
    }

    #[test]
    fn find_route_by_method_and_path() {
        clear_routes();
        register_routes(vec![
            RouteInfo {
                method: "GET".to_string(),
                path: "/users".to_string(),
                circuit_name: Some("UserList".to_string()),
                input_schema: None,
                output_schema: None,
            },
            RouteInfo {
                method: "POST".to_string(),
                path: "/users".to_string(),
                circuit_name: Some("CreateUser".to_string()),
                input_schema: None,
                output_schema: None,
            },
        ]);

        let found = find_route("POST", "/users");
        assert!(found.is_some());
        assert_eq!(found.unwrap().circuit_name.as_deref(), Some("CreateUser"));

        assert!(find_route("DELETE", "/users").is_none());
    }
}
