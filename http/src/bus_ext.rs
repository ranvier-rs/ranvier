//! # HTTP-Specific Bus Extensions
//!
//! Convenience methods for extracting HTTP-specific types (PathParams, QueryParams)
//! from the Bus. These belong in `ranvier-http` (not `ranvier-core`) because they
//! reference protocol-specific types, preserving Core's protocol-agnosticism.
//!
//! ## Design Rationale
//!
//! The Bus in `ranvier-core` is protocol-agnostic and uses type-indexed storage.
//! PathParams and QueryParams are HTTP concepts defined in `ranvier-http`.
//! Extension traits allow a clean API (`bus.path_param("id")`) without
//! coupling the core framework to HTTP semantics.

use std::str::FromStr;

use ranvier_core::Bus;
use serde::Serialize;

use crate::ingress::{PathParams, QueryParams};

/// HTTP-specific convenience methods for [`Bus`].
///
/// Import this trait to access path/query parameter extraction directly from the Bus.
///
/// # Example
///
/// ```rust,ignore
/// use ranvier_http::BusHttpExt;
/// use uuid::Uuid;
///
/// let id: Uuid = ranvier_core::try_outcome!(bus.path_param("id"), "path");
/// let page: i64 = bus.query_param_or("page", 1);
/// ```
pub trait BusHttpExt {
    /// Extract and parse a path parameter from the Bus.
    ///
    /// Looks up [`PathParams`] in the Bus, then parses the named parameter as `T`.
    ///
    /// Returns `Err` if:
    /// - `PathParams` is not in the Bus
    /// - The named key does not exist in PathParams
    /// - The value cannot be parsed as `T`
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use ranvier_http::BusHttpExt;
    /// use uuid::Uuid;
    ///
    /// // Inside a transition:
    /// let id: Uuid = ranvier_core::try_outcome!(bus.path_param("id"), "path");
    /// ```
    fn path_param<T: FromStr>(&self, name: &str) -> Result<T, String>;

    /// Extract and parse a query parameter from the Bus.
    ///
    /// Returns `None` if `QueryParams` is missing, the key is absent, or parsing fails.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use ranvier_http::BusHttpExt;
    ///
    /// let page: Option<i64> = bus.query_param("page");
    /// ```
    fn query_param<T: FromStr>(&self, name: &str) -> Option<T>;

    /// Extract and parse a query parameter, or return a default value.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use ranvier_http::BusHttpExt;
    ///
    /// let page: i64 = bus.query_param_or("page", 1);
    /// let per_page: i64 = bus.query_param_or("per_page", 20);
    /// ```
    fn query_param_or<T: FromStr>(&self, name: &str, default: T) -> T;
}

impl BusHttpExt for Bus {
    fn path_param<T: FromStr>(&self, name: &str) -> Result<T, String> {
        self.read::<PathParams>()
            .and_then(|p| p.get_parsed::<T>(name))
            .ok_or_else(|| format!("Missing or invalid path parameter: {name}"))
    }

    fn query_param<T: FromStr>(&self, name: &str) -> Option<T> {
        self.read::<QueryParams>()
            .and_then(|q| q.get_parsed::<T>(name))
    }

    fn query_param_or<T: FromStr>(&self, name: &str, default: T) -> T {
        self.query_param(name).unwrap_or(default)
    }
}

/// Create an `Outcome::Next` with a JSON-serialized value, or `Outcome::Fault` on error.
///
/// This is a convenience function for the common pattern of serializing a response
/// to JSON and wrapping it in `Outcome::Next`. Placed in `ranvier-http` (not `ranvier-core`)
/// because JSON serialization is a protocol-boundary concern.
///
/// # Example
///
/// ```rust,ignore
/// use ranvier_http::json_outcome;
/// use serde::Serialize;
///
/// #[derive(Serialize)]
/// struct ApiResponse { status: String }
///
/// let response = ApiResponse { status: "ok".into() };
/// let outcome = json_outcome(&response);
/// // outcome == Outcome::Next(r#"{"status":"ok"}"#.to_string())
/// ```
pub fn json_outcome<T: Serialize>(value: &T) -> ranvier_core::Outcome<String, String> {
    match serde_json::to_string(value) {
        Ok(json) => ranvier_core::Outcome::Next(json),
        Err(e) => ranvier_core::Outcome::Fault(format!("JSON serialization failed: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ranvier_core::Bus;
    use std::collections::HashMap;

    #[test]
    fn path_param_parses_uuid() {
        let mut bus = Bus::new();
        let mut values = HashMap::new();
        values.insert(
            "id".to_string(),
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
        );
        bus.insert(PathParams::new(values));

        let id: uuid::Uuid = bus.path_param("id").unwrap();
        assert_eq!(id.to_string(), "550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn path_param_parses_i64() {
        let mut bus = Bus::new();
        let mut values = HashMap::new();
        values.insert("page".to_string(), "42".to_string());
        bus.insert(PathParams::new(values));

        let page: i64 = bus.path_param("page").unwrap();
        assert_eq!(page, 42);
    }

    #[test]
    fn path_param_missing_returns_err() {
        let bus = Bus::new();
        let result: Result<i64, String> = bus.path_param("id");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Missing or invalid"));
    }

    #[test]
    fn path_param_invalid_parse_returns_err() {
        let mut bus = Bus::new();
        let mut values = HashMap::new();
        values.insert("id".to_string(), "not-a-uuid".to_string());
        bus.insert(PathParams::new(values));

        let result: Result<uuid::Uuid, String> = bus.path_param("id");
        assert!(result.is_err());
    }

    #[test]
    fn query_param_parses_value() {
        let mut bus = Bus::new();
        bus.insert(QueryParams::from_query("page=5&limit=20"));

        let page: Option<i64> = bus.query_param("page");
        assert_eq!(page, Some(5));

        let limit: Option<i64> = bus.query_param("limit");
        assert_eq!(limit, Some(20));
    }

    #[test]
    fn query_param_missing_returns_none() {
        let mut bus = Bus::new();
        bus.insert(QueryParams::from_query("page=1"));

        let missing: Option<i64> = bus.query_param("nonexistent");
        assert!(missing.is_none());
    }

    #[test]
    fn query_param_no_query_params_in_bus_returns_none() {
        let bus = Bus::new();
        let result: Option<i64> = bus.query_param("page");
        assert!(result.is_none());
    }

    #[test]
    fn query_param_or_returns_parsed_value() {
        let mut bus = Bus::new();
        bus.insert(QueryParams::from_query("page=3"));

        let page: i64 = bus.query_param_or("page", 1);
        assert_eq!(page, 3);
    }

    #[test]
    fn query_param_or_returns_default_when_missing() {
        let mut bus = Bus::new();
        bus.insert(QueryParams::from_query(""));

        let page: i64 = bus.query_param_or("page", 1);
        assert_eq!(page, 1);

        let per_page: i64 = bus.query_param_or("per_page", 20);
        assert_eq!(per_page, 20);
    }

    #[test]
    fn json_outcome_success() {
        #[derive(Serialize)]
        struct Resp {
            status: String,
        }
        let resp = Resp {
            status: "ok".into(),
        };
        let outcome = json_outcome(&resp);
        assert!(outcome.is_next());
        match outcome {
            ranvier_core::Outcome::Next(json) => {
                assert!(json.contains("\"status\""));
                assert!(json.contains("\"ok\""));
            }
            _ => panic!("Expected Next"),
        }
    }

    #[test]
    fn json_outcome_with_vec() {
        let items = vec![1, 2, 3];
        let outcome = json_outcome(&items);
        assert!(outcome.is_next());
        match outcome {
            ranvier_core::Outcome::Next(json) => {
                assert_eq!(json, "[1,2,3]");
            }
            _ => panic!("Expected Next"),
        }
    }
}
