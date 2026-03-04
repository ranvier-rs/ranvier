use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use ranvier_core::transition::ResourceRequirement;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::ingress::{HttpIngress, RawIngressService};

#[derive(Debug, thiserror::Error)]
pub enum TestHarnessError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("hyper error: {0}")]
    Hyper(#[from] hyper::Error),
    #[error("task join error: {0}")]
    Join(#[from] tokio::task::JoinError),
    #[error("invalid response: {0}")]
    InvalidResponse(&'static str),
    #[error("invalid utf-8 in response headers: {0}")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("invalid status code text: {0}")]
    InvalidStatus(#[from] std::num::ParseIntError),
    #[error("invalid status code value: {0}")]
    InvalidStatusCode(#[from] http::status::InvalidStatusCode),
    #[error("invalid header name: {0}")]
    InvalidHeaderName(#[from] http::header::InvalidHeaderName),
    #[error("invalid header value: {0}")]
    InvalidHeaderValue(#[from] http::header::InvalidHeaderValue),
    #[error("json serialization error: {0}")]
    Json(#[from] serde_json::Error),
}

/// In-process HTTP test harness for `HttpIngress`.
///
/// Uses an in-memory duplex stream and Hyper HTTP/1.1 server connection,
/// so no TCP socket/network bind is required.
#[derive(Clone)]
pub struct TestApp<R> {
    service: RawIngressService<R>,
    host: String,
}

impl<R> TestApp<R>
where
    R: ResourceRequirement + Clone + Send + Sync + 'static,
{
    pub fn new(ingress: HttpIngress<R>, resources: R) -> Self {
        Self {
            service: ingress.into_raw_service(resources),
            host: "test.local".to_string(),
        }
    }

    pub fn with_host(mut self, host: impl Into<String>) -> Self {
        self.host = host.into();
        self
    }

    pub async fn send(&self, request: TestRequest) -> Result<TestResponse, TestHarnessError> {
        let mut request_bytes = request.to_http1_bytes(&self.host);
        let capacity = request_bytes.len().saturating_mul(2).max(16 * 1024);
        let (mut client_io, server_io) = tokio::io::duplex(capacity);

        let service = self.service.clone();
        let server_task = tokio::spawn(async move {
            let hyper_service = TowerToHyperService::new(service);
            http1::Builder::new()
                .keep_alive(false)
                .serve_connection(TokioIo::new(server_io), hyper_service)
                .await
        });

        client_io.write_all(&request_bytes).await?;

        let mut raw_response = Vec::new();
        client_io.read_to_end(&mut raw_response).await?;

        let response = TestResponse::from_http1_bytes(&raw_response)?;

        // Connection close races in in-memory duplex mode can surface as
        // IncompleteMessage after a valid response is already produced.
        // Treat that specific case as non-fatal for test harness usage.
        let server_result = server_task.await?;
        if let Err(error) = server_result {
            if !error.is_incomplete_message() {
                return Err(TestHarnessError::Hyper(error));
            }
        }

        // Avoid keeping oversized request buffer alive longer than needed.
        request_bytes.clear();

        Ok(response)
    }
}

#[derive(Clone, Debug)]
pub struct TestRequest {
    method: Method,
    path: String,
    headers: Vec<(String, String)>,
    body: Bytes,
}

impl TestRequest {
    pub fn new(method: Method, path: impl Into<String>) -> Self {
        Self {
            method,
            path: path.into(),
            headers: Vec::new(),
            body: Bytes::new(),
        }
    }

    pub fn get(path: impl Into<String>) -> Self {
        Self::new(Method::GET, path)
    }

    pub fn post(path: impl Into<String>) -> Self {
        Self::new(Method::POST, path)
    }

    pub fn put(path: impl Into<String>) -> Self {
        Self::new(Method::PUT, path)
    }

    pub fn delete(path: impl Into<String>) -> Self {
        Self::new(Method::DELETE, path)
    }

    pub fn patch(path: impl Into<String>) -> Self {
        Self::new(Method::PATCH, path)
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn body(mut self, body: impl Into<Bytes>) -> Self {
        self.body = body.into();
        self
    }

    pub fn text(mut self, body: impl Into<String>) -> Self {
        self.body = Bytes::from(body.into());
        self
    }

    pub fn json<T: Serialize>(mut self, payload: &T) -> Result<Self, TestHarnessError> {
        self.body = Bytes::from(serde_json::to_vec(payload)?);
        self.headers
            .push(("content-type".to_string(), "application/json".to_string()));
        Ok(self)
    }

    fn to_http1_bytes(&self, host: &str) -> Vec<u8> {
        let path = if self.path.is_empty() {
            "/"
        } else {
            &self.path
        };

        let mut has_host = false;
        let mut has_connection = false;
        let mut has_content_length = false;

        for (name, _) in &self.headers {
            let lower = name.to_ascii_lowercase();
            if lower == "host" {
                has_host = true;
            } else if lower == "connection" {
                has_connection = true;
            } else if lower == "content-length" {
                has_content_length = true;
            }
        }

        let mut output = format!("{} {} HTTP/1.1\r\n", self.method, path);

        if !has_host {
            output.push_str(&format!("Host: {host}\r\n"));
        }
        if !has_connection {
            output.push_str("Connection: close\r\n");
        }
        if !has_content_length {
            output.push_str(&format!("Content-Length: {}\r\n", self.body.len()));
        }

        for (name, value) in &self.headers {
            output.push_str(name);
            output.push_str(": ");
            output.push_str(value);
            output.push_str("\r\n");
        }

        output.push_str("\r\n");

        let mut bytes = output.into_bytes();
        bytes.extend_from_slice(&self.body);
        bytes
    }
}

#[derive(Clone, Debug)]
pub struct TestResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
}

impl TestResponse {
    fn from_http1_bytes(raw: &[u8]) -> Result<Self, TestHarnessError> {
        let delimiter = b"\r\n\r\n";
        let header_end = raw
            .windows(delimiter.len())
            .position(|window| window == delimiter)
            .ok_or(TestHarnessError::InvalidResponse(
                "missing HTTP header delimiter",
            ))?;

        let header_text = std::str::from_utf8(&raw[..header_end])?;
        let mut lines = header_text.split("\r\n");

        let status_line = lines
            .next()
            .ok_or(TestHarnessError::InvalidResponse("missing status line"))?;
        let mut status_parts = status_line.split_whitespace();
        let _http_version = status_parts
            .next()
            .ok_or(TestHarnessError::InvalidResponse("missing HTTP version"))?;
        let status_code = status_parts
            .next()
            .ok_or(TestHarnessError::InvalidResponse("missing status code"))?
            .parse::<u16>()?;
        let status = StatusCode::from_u16(status_code)?;

        let mut headers = HeaderMap::new();
        for line in lines {
            if line.is_empty() {
                continue;
            }
            let (name, value) = line
                .split_once(':')
                .ok_or(TestHarnessError::InvalidResponse("malformed header line"))?;
            let name = HeaderName::from_bytes(name.trim().as_bytes())?;
            let value = HeaderValue::from_str(value.trim())?;
            headers.append(name, value);
        }

        let body = Bytes::copy_from_slice(&raw[(header_end + delimiter.len())..]);

        Ok(Self {
            status,
            headers,
            body,
        })
    }

    pub fn status(&self) -> StatusCode {
        self.status
    }

    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    pub fn header(&self, name: &str) -> Option<&HeaderValue> {
        self.headers.get(name)
    }

    pub fn body(&self) -> &[u8] {
        &self.body
    }

    pub fn text(&self) -> Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.body)
    }

    pub fn json<T: DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_slice(&self.body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ranvier_core::{Outcome, Transition};
    use ranvier_runtime::Axon;

    #[derive(Clone)]
    struct Ping;

    #[async_trait::async_trait]
    impl Transition<(), String> for Ping {
        type Error = String;
        type Resources = ();

        async fn run(
            &self,
            _state: (),
            _resources: &Self::Resources,
            _bus: &mut ranvier_core::Bus,
        ) -> Outcome<String, Self::Error> {
            Outcome::next("pong".to_string())
        }
    }

    #[tokio::test]
    async fn test_app_executes_route_without_network_socket() {
        let ingress = crate::Ranvier::http::<()>().get(
            "/ping",
            Axon::<(), (), String, ()>::new("Ping").then(Ping),
        );
        let app = TestApp::new(ingress, ());

        let response = app
            .send(TestRequest::get("/ping"))
            .await
            .expect("test request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.text().expect("utf8 body"), "pong");
    }
}
