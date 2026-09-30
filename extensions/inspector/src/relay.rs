use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

/// Initial response budget aligned with the workspace HTTP body limit.
pub const MAX_RELAY_RESPONSE_BYTES: usize = 1024 * 1024;

/// Configuration for the Inspector relay proxy.
#[derive(Clone)]
pub struct RelayConfig {
    /// Target base URL for the running application server (e.g., `http://127.0.0.1:3111`)
    pub target_url: String,
    /// Timeout per relay request in milliseconds (default: 30000)
    pub timeout_ms: u64,
    /// Maximum concurrent relay requests (default: 10)
    pub max_concurrent: usize,
}
impl std::fmt::Debug for RelayConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayConfig")
            .field("target_url", &"[REDACTED]")
            .field("timeout_ms", &self.timeout_ms)
            .field("max_concurrent", &self.max_concurrent)
            .finish()
    }
}

impl RelayConfig {
    pub fn new(target_url: impl Into<String>) -> Result<Self, RelayConfigError> {
        Self::from_values(
            target_url.into(),
            env_value("RANVIER_INSPECTOR_RELAY_TIMEOUT_MS")?.as_deref(),
            env_value("RANVIER_INSPECTOR_RELAY_MAX_CONCURRENT")?.as_deref(),
        )
    }

    fn from_values(
        target_url: String,
        timeout: Option<&str>,
        concurrency: Option<&str>,
    ) -> Result<Self, RelayConfigError> {
        let config = Self {
            target_url,
            timeout_ms: parse_budget(timeout, 30000)?,
            max_concurrent: usize::try_from(parse_budget(concurrency, 10)?)
                .map_err(|_| RelayConfigError::InvalidBudget)?,
        };
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), RelayConfigError> {
        let url =
            reqwest::Url::parse(&self.target_url).map_err(|_| RelayConfigError::InvalidTarget)?;
        if self.target_url.trim() != self.target_url
            || self.target_url.chars().any(char::is_control)
            || !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(RelayConfigError::InvalidTarget);
        }
        if self.timeout_ms == 0
            || self.max_concurrent == 0
            || self.max_concurrent > Semaphore::MAX_PERMITS
        {
            return Err(RelayConfigError::InvalidBudget);
        }
        Ok(())
    }
}

/// Sanitized startup errors; caller input and client errors are never retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayConfigError {
    InvalidEnvironment,
    InvalidTarget,
    InvalidBudget,
    ClientBuild,
}
impl std::fmt::Display for RelayConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "relay configuration error: {self:?}")
    }
}
impl std::error::Error for RelayConfigError {}
fn env_value(key: &str) -> Result<Option<String>, RelayConfigError> {
    match std::env::var(key) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(_) => Err(RelayConfigError::InvalidEnvironment),
    }
}
fn parse_budget(value: Option<&str>, default: u64) -> Result<u64, RelayConfigError> {
    match value {
        None => Ok(default),
        Some(value) if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) => {
            value
                .parse::<u64>()
                .ok()
                .filter(|value| *value > 0)
                .ok_or(RelayConfigError::InvalidBudget)
        }
        Some(_) => Err(RelayConfigError::InvalidBudget),
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
    pub fn new(config: RelayConfig) -> Result<Self, RelayConfigError> {
        config.validate()?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(config.timeout_ms))
            .build()
            .map_err(|_| RelayConfigError::ClientBuild)?;
        let semaphore = Arc::new(Semaphore::new(config.max_concurrent));
        Ok(Self {
            config,
            client,
            semaphore,
        })
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
    /// HTTP status of the relay operation failure, distinct from upstream status.
    pub fn status_code(&self) -> axum::http::StatusCode {
        use axum::http::StatusCode;
        match self.error.as_str() {
            "concurrency_limit" => StatusCode::SERVICE_UNAVAILABLE,
            "timeout" => StatusCode::GATEWAY_TIMEOUT,
            "invalid_request" => StatusCode::BAD_REQUEST,
            _ => StatusCode::BAD_GATEWAY,
        }
    }
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
        .map_err(|_| RelayError {
            error: "invalid_request".into(),
            message: "Invalid relay method".into(),
            details: None,
        })?;

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
        let bytes = headers.iter().try_fold(0usize, |total, (key, value)| {
            total
                .checked_add(key.len())
                .and_then(|total| total.checked_add(value.len()))
        });
        if headers.len() > 64 || bytes.is_none_or(|bytes| bytes > 8192) {
            return Err(invalid_header());
        }
        for (key, value) in headers {
            let name = key
                .parse::<reqwest::header::HeaderName>()
                .map_err(|_| invalid_header())?;
            let value = value
                .parse::<reqwest::header::HeaderValue>()
                .map_err(|_| invalid_header())?;
            builder = builder.header(name, value);
        }
    }

    if let Some(body) = &req.body {
        builder = builder
            .header("content-type", "application/json")
            .json(body);
    }

    let mut response = builder.send().await.map_err(|e| {
        if e.is_timeout() {
            RelayError::timeout(relay.config.timeout_ms)
        } else if e.is_connect() {
            RelayError::connection_refused("configured relay target")
        } else {
            RelayError::internal("Relay request failed")
        }
    })?;

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

    if response
        .content_length()
        .is_some_and(|size| size > MAX_RELAY_RESPONSE_BYTES as u64)
    {
        return Err(response_too_large());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        if error.is_timeout() {
            RelayError::timeout(relay.config.timeout_ms)
        } else {
            RelayError::internal("Relay response read failed")
        }
    })? {
        append_response_chunk(&mut bytes, &chunk)?;
    }
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .map_err(|_| RelayError::internal("Relay response is not valid JSON"))?
    };
    let duration_ms = start.elapsed().as_millis().min(u64::MAX as u128) as u64;

    Ok(RelayResponse {
        status,
        headers: resp_headers,
        body,
        duration_ms,
        trace_id,
    })
}

fn response_too_large() -> RelayError {
    RelayError {
        error: "response_too_large".into(),
        message: "Relay response exceeds body budget".into(),
        details: None,
    }
}
fn invalid_header() -> RelayError {
    RelayError {
        error: "invalid_request".into(),
        message: "Invalid relay header".into(),
        details: None,
    }
}
fn append_response_chunk(bytes: &mut Vec<u8>, chunk: &[u8]) -> Result<(), RelayError> {
    if chunk.len() > MAX_RELAY_RESPONSE_BYTES.saturating_sub(bytes.len()) {
        return Err(response_too_large());
    }
    bytes.extend_from_slice(chunk);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn malformed_or_oversized_headers_fail_before_network_and_release_permit() {
        let relay = RelayState::new(RelayConfig {
            target_url: "http://127.0.0.1:1".into(),
            timeout_ms: 100,
            max_concurrent: 1,
        })
        .unwrap();
        let cases = [
            std::collections::HashMap::from([("bad name".into(), "SENTINEL".into())]),
            std::collections::HashMap::from([(
                "x-test".into(),
                "SENTINEL\r\ninjected: value".into(),
            )]),
            std::collections::HashMap::from([("x-test".into(), "x".repeat(8192))]),
            (0..65)
                .map(|index| (format!("x-{index}"), "value".into()))
                .collect(),
        ];
        for headers in cases {
            let error = execute_relay(
                &relay,
                RelayRequest {
                    method: "GET".into(),
                    path: "/".into(),
                    headers: Some(headers),
                    body: None,
                },
            )
            .await
            .unwrap_err();
            assert_eq!(error.status_code(), axum::http::StatusCode::BAD_REQUEST);
            assert_eq!(error.message, "Invalid relay header");
            assert!(!serde_json::to_string(&error).unwrap().contains("SENTINEL"));
            assert_eq!(relay.semaphore.available_permits(), 1);
        }
    }

    #[test]
    fn relay_http_error_statuses_distinguish_operation_failures() {
        use axum::http::StatusCode;
        assert_eq!(
            RelayError::concurrency_limit().status_code(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            RelayError::timeout(100).status_code(),
            StatusCode::GATEWAY_TIMEOUT
        );
        assert_eq!(response_too_large().status_code(), StatusCode::BAD_GATEWAY);
        assert_eq!(
            RelayError::internal("sanitized").status_code(),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            RelayError {
                error: "future_error".into(),
                message: String::new(),
                details: None
            }
            .status_code(),
            StatusCode::BAD_GATEWAY
        );
    }

    #[tokio::test]
    async fn invalid_method_returns_bad_request_and_releases_permit() {
        let relay = RelayState::new(RelayConfig {
            target_url: "http://127.0.0.1:1".into(),
            timeout_ms: 100,
            max_concurrent: 1,
        })
        .unwrap();
        let error = execute_relay(
            &relay,
            RelayRequest {
                method: "invalid method SENTINEL".into(),
                path: "/".into(),
                headers: None,
                body: None,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.status_code(), axum::http::StatusCode::BAD_REQUEST);
        assert!(!serde_json::to_string(&error).unwrap().contains("SENTINEL"));
        assert_eq!(relay.semaphore.available_permits(), 1);
        let permit = relay.semaphore.acquire().await.unwrap();
        let busy = execute_relay(
            &relay,
            RelayRequest {
                method: "GET".into(),
                path: "/".into(),
                headers: None,
                body: None,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(
            busy.status_code(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
        drop(permit);
    }

    #[test]
    fn response_chunk_budget_rejects_growth_without_partial_append() {
        let mut bytes = vec![0; MAX_RELAY_RESPONSE_BYTES - 1];
        append_response_chunk(&mut bytes, &[1]).unwrap();
        assert_eq!(
            append_response_chunk(&mut bytes, &[2]).unwrap_err().error,
            "response_too_large"
        );
        assert_eq!(bytes.len(), MAX_RELAY_RESPONSE_BYTES);
    }

    #[tokio::test]
    async fn chunked_overflow_and_stalled_body_release_relay_permit() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for stalled in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut socket, _) =
                    tokio::time::timeout(Duration::from_secs(3), listener.accept())
                        .await
                        .unwrap()
                        .unwrap();
                let mut request = [0; 4096];
                assert!(socket.read(&mut request).await.unwrap() > 0);
                socket.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").await.unwrap();
                if stalled {
                    // Hold the body open past the client's complete-request budget.
                    tokio::time::sleep(Duration::from_secs(2)).await;
                } else {
                    let chunk = vec![b'x'; 64 * 1024];
                    for _ in 0..17 {
                        if socket.write_all(b"10000\r\n").await.is_err()
                            || socket.write_all(&chunk).await.is_err()
                            || socket.write_all(b"\r\n").await.is_err()
                        {
                            break;
                        }
                    }
                    let _ = socket.write_all(b"0\r\n\r\n").await;
                }
            });
            let relay = RelayState::new(RelayConfig {
                target_url: format!("http://{address}"),
                timeout_ms: if stalled { 100 } else { 2000 },
                max_concurrent: 1,
            })
            .unwrap();
            let started = std::time::Instant::now();
            let result = tokio::time::timeout(
                Duration::from_secs(3),
                execute_relay(
                    &relay,
                    RelayRequest {
                        method: "GET".into(),
                        path: "/".into(),
                        headers: None,
                        body: None,
                    },
                ),
            )
            .await
            .unwrap();
            let elapsed = started.elapsed();
            server.await.unwrap();
            assert_eq!(
                result.unwrap_err().error,
                if stalled {
                    "timeout"
                } else {
                    "response_too_large"
                }
            );
            if stalled {
                assert!(elapsed < Duration::from_millis(1500));
            }
            assert_eq!(relay.semaphore.available_permits(), 1);
        }
    }

    #[tokio::test]
    async fn relay_reads_actual_http_body_and_rejects_invalid_or_oversized_payload() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for (body, length, expected) in [
            ("{\"ok\":true}", 11, None),
            ("invalid", 7, Some("internal_error")),
            ("", MAX_RELAY_RESPONSE_BYTES + 1, Some("response_too_large")),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut socket, _) =
                    tokio::time::timeout(Duration::from_secs(3), listener.accept())
                        .await
                        .unwrap()
                        .unwrap();
                let mut request = [0; 4096];
                let received = socket.read(&mut request).await.unwrap();
                assert!(received > 0, "loopback client sent no request");
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n{body}"
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            });
            let relay = RelayState::new(RelayConfig {
                target_url: format!("http://{address}"),
                timeout_ms: 1000,
                max_concurrent: 1,
            })
            .unwrap();
            let result = execute_relay(
                &relay,
                RelayRequest {
                    method: "GET".into(),
                    path: "/".into(),
                    headers: None,
                    body: None,
                },
            )
            .await;
            match expected {
                None => assert_eq!(result.unwrap().body, serde_json::json!({"ok":true})),
                Some(error) => assert_eq!(result.unwrap_err().error, error),
            }
            assert_eq!(relay.semaphore.available_permits(), 1);
            server.await.unwrap();
        }
    }

    #[test]
    fn relay_budget_parser_rejects_invalid_explicit_values() {
        assert_eq!(parse_budget(None, 10), Ok(10));
        assert_eq!(parse_budget(Some("42"), 10), Ok(42));
        for value in [
            "",
            "0",
            "-1",
            "+1",
            " 1",
            "1 ",
            "1.0",
            "18446744073709551616",
            "secret",
        ] {
            assert_eq!(
                parse_budget(Some(value), 10),
                Err(RelayConfigError::InvalidBudget)
            );
        }
    }

    #[test]
    fn relay_startup_validates_target_and_direct_budget_configuration() {
        for target in [
            "ftp://host",
            "http://user:secret@host",
            "http://host?secret=value",
            "http://host#secret",
            " http://host",
            "http://host\n",
            "invalid",
        ] {
            assert_eq!(
                RelayConfig::from_values(target.into(), None, None).unwrap_err(),
                RelayConfigError::InvalidTarget
            );
        }
        for (timeout_ms, max_concurrent) in [(0, 1), (1, 0), (1, Semaphore::MAX_PERMITS + 1)] {
            let config = RelayConfig {
                target_url: "http://localhost".into(),
                timeout_ms,
                max_concurrent,
            };
            assert!(matches!(
                RelayState::new(config),
                Err(RelayConfigError::InvalidBudget)
            ));
        }
    }

    #[test]
    fn relay_config_debug_and_errors_do_not_echo_target_secrets() {
        let config = RelayConfig {
            target_url: "http://user:SENTINEL@host".into(),
            timeout_ms: 1,
            max_concurrent: 1,
        };
        assert!(!format!("{config:?}").contains("SENTINEL"));
        let error = config.validate().unwrap_err();
        assert!(!format!("{error:?} {error}").contains("SENTINEL"));
    }

    #[test]
    fn relay_config_defaults() {
        let config = RelayConfig::from_values("http://127.0.0.1:3111".into(), None, None).unwrap();
        assert_eq!(config.target_url, "http://127.0.0.1:3111");
        assert_eq!(config.max_concurrent, 10);
    }

    #[test]
    fn relay_state_uses_configured_concurrency_limit() {
        let state = RelayState::new(RelayConfig {
            target_url: "http://127.0.0.1:3111".to_string(),
            timeout_ms: 1_000,
            max_concurrent: 3,
        })
        .unwrap();

        assert_eq!(state.semaphore.available_permits(), 3);
    }

    #[test]
    fn relay_error_serializes() {
        let err = RelayError::timeout(30000);
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["error"], "timeout");
        assert!(json["message"].as_str().unwrap().contains("30000"));
    }
}
