use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

/// Configuration for the Inspector relay proxy.
#[derive(Clone, Debug)]
pub struct RelayConfig {
    /// Target base URL for the running application server (e.g., `http://127.0.0.1:3111`)
    pub target_url: String,
    /// Timeout per relay request in milliseconds (default: 30000)
    pub timeout_ms: u64,
    /// Maximum concurrent relay requests (default: 10)
    pub max_concurrent: usize,
}

impl RelayConfig {
    pub fn new(target_url: impl Into<String>) -> Self {
        let timeout_ms = std::env::var("RANVIER_INSPECTOR_RELAY_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30000);

        Self {
            target_url: target_url.into(),
            timeout_ms,
            max_concurrent: 10,
        }
    }
}

/// Shared relay state for the Inspector endpoints.
#[derive(Clone)]
pub struct RelayState {
    pub config: RelayConfig,
    pub client: reqwest::Client,
    pub semaphore: Arc<Semaphore>,
}

impl RelayState {
    pub fn new(config: RelayConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(config.timeout_ms))
            .build()
            .unwrap_or_default();
        let semaphore = Arc::new(Semaphore::new(config.max_concurrent));
        Self {
            config,
            client,
            semaphore,
        }
    }
}

/// Request body for `POST /api/v1/relay`.
#[derive(Debug, Deserialize)]
pub struct RelayRequest {
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub headers: Option<std::collections::HashMap<String, String>>,
    #[serde(default)]
    pub body: Option<Value>,
}

/// Response from `POST /api/v1/relay`.
#[derive(Debug, Serialize)]
pub struct RelayResponse {
    pub status: u16,
    pub headers: std::collections::HashMap<String, String>,
    pub body: Value,
    pub duration_ms: u64,
    pub trace_id: Option<String>,
}

/// Error response format for relay failures.
#[derive(Debug, Serialize)]
pub struct RelayError {
    pub error: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

impl RelayError {
    pub fn connection_refused(target: &str) -> Self {
        Self {
            error: "connection_refused".to_string(),
            message: format!("Could not connect to target server at {target}"),
            details: None,
        }
    }

    pub fn timeout(timeout_ms: u64) -> Self {
        Self {
            error: "timeout".to_string(),
            message: format!("Request timed out after {timeout_ms}ms"),
            details: None,
        }
    }

    pub fn concurrency_limit() -> Self {
        Self {
            error: "concurrency_limit".to_string(),
            message: "Maximum concurrent relay requests reached".to_string(),
            details: None,
        }
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Self {
            error: "internal_error".to_string(),
            message: msg.into(),
            details: None,
        }
    }
}

/// Execute a relay request through the proxy.
pub async fn execute_relay(
    relay: &RelayState,
    req: RelayRequest,
) -> Result<RelayResponse, RelayError> {
    // Acquire concurrency permit
    let _permit = relay
        .semaphore
        .clone()
        .try_acquire_owned()
        .map_err(|_| RelayError::concurrency_limit())?;

    let method = req
        .method
        .parse::<reqwest::Method>()
        .map_err(|e| RelayError::internal(format!("Invalid method: {e}")))?;

    let url = format!(
        "{}{}",
        relay.config.target_url.trim_end_matches('/'),
        if req.path.starts_with('/') {
            req.path.clone()
        } else {
            format!("/{}", req.path)
        }
    );

    let start = std::time::Instant::now();

    let mut builder = relay.client.request(method, &url);

    if let Some(headers) = &req.headers {
        for (key, value) in headers {
            if let Ok(name) = key.parse::<reqwest::header::HeaderName>() {
                if let Ok(val) = value.parse::<reqwest::header::HeaderValue>() {
                    builder = builder.header(name, val);
                }
            }
        }
    }

    if let Some(body) = &req.body {
        builder = builder
            .header("content-type", "application/json")
            .json(body);
    }

    let response = builder.send().await.map_err(|e| {
        if e.is_timeout() {
            RelayError::timeout(relay.config.timeout_ms)
        } else if e.is_connect() {
            RelayError::connection_refused(&url)
        } else {
            RelayError::internal(format!("Request failed: {e}"))
        }
    })?;

    let duration_ms = start.elapsed().as_millis() as u64;
    let status = response.status().as_u16();

    let mut resp_headers = std::collections::HashMap::new();
    let trace_id = response
        .headers()
        .get("x-request-id")
        .or(response.headers().get("x-trace-id"))
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    for (key, value) in response.headers() {
        if let Ok(v) = value.to_str() {
            resp_headers.insert(key.to_string(), v.to_string());
        }
    }

    let body = response.json::<Value>().await.unwrap_or(Value::Null);

    Ok(RelayResponse {
        status,
        headers: resp_headers,
        body,
        duration_ms,
        trace_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_config_defaults() {
        let config = RelayConfig::new("http://127.0.0.1:3111");
        assert_eq!(config.target_url, "http://127.0.0.1:3111");
        assert_eq!(config.max_concurrent, 10);
    }

    #[test]
    fn relay_error_serializes() {
        let err = RelayError::timeout(30000);
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["error"], "timeout");
        assert!(json["message"].as_str().unwrap().contains("30000"));
    }
}
