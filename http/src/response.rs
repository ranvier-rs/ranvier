use bytes::Bytes;
use http::header::CONTENT_TYPE;
use http::{Response, StatusCode};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use ranvier_core::Outcome;
use serde::Serialize;
use std::convert::Infallible;

use crate::extract::Json;

pub type HttpResponse = Response<BoxBody<Bytes, Infallible>>;

pub trait IntoResponse {
    fn into_response(self) -> HttpResponse;
}

pub fn json_error_response(status: StatusCode, message: impl Into<String>) -> HttpResponse {
    let payload = serde_json::json!({ "error": message.into() });
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json")
        .body(
            Full::new(Bytes::from(payload.to_string()))
                .map_err(|never| match never {})
                .boxed(),
        )
        .expect("response builder should be infallible")
}

/// HTML response wrapper.
///
/// Wraps a string body with `Content-Type: text/html; charset=utf-8`.
///
/// # Example
///
/// ```rust,ignore
/// Outcome::next(Html("<h1>Hello</h1>".to_string()))
/// ```
#[derive(Debug, Clone)]
pub struct Html(pub String);

impl IntoResponse for Html {
    fn into_response(self) -> HttpResponse {
        Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "text/html; charset=utf-8")
            .body(
                Full::new(Bytes::from(self.0))
                    .map_err(|never| match never {})
                    .boxed(),
            )
            .expect("response builder should be infallible")
    }
}

impl IntoResponse for (StatusCode, Html) {
    fn into_response(self) -> HttpResponse {
        Response::builder()
            .status(self.0)
            .header(CONTENT_TYPE, "text/html; charset=utf-8")
            .body(
                Full::new(Bytes::from((self.1).0))
                    .map_err(|never| match never {})
                    .boxed(),
            )
            .expect("response builder should be infallible")
    }
}

impl IntoResponse for HttpResponse {
    fn into_response(self) -> HttpResponse {
        self
    }
}

impl IntoResponse for String {
    fn into_response(self) -> HttpResponse {
        Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(
                Full::new(Bytes::from(self))
                    .map_err(|never| match never {})
                    .boxed(),
            )
            .expect("response builder should be infallible")
    }
}

impl IntoResponse for &'static str {
    fn into_response(self) -> HttpResponse {
        Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(
                Full::new(Bytes::from(self))
                    .map_err(|never| match never {})
                    .boxed(),
            )
            .expect("response builder should be infallible")
    }
}

impl IntoResponse for Bytes {
    fn into_response(self) -> HttpResponse {
        Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "application/octet-stream")
            .body(Full::new(self).map_err(|never| match never {}).boxed())
            .expect("response builder should be infallible")
    }
}

impl IntoResponse for serde_json::Value {
    fn into_response(self) -> HttpResponse {
        Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "application/json")
            .body(
                Full::new(Bytes::from(self.to_string()))
                    .map_err(|never| match never {})
                    .boxed(),
            )
            .expect("response builder should be infallible")
    }
}

impl IntoResponse for () {
    fn into_response(self) -> HttpResponse {
        Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(
                Full::new(Bytes::new())
                    .map_err(|never| match never {})
                    .boxed(),
            )
            .expect("response builder should be infallible")
    }
}

impl IntoResponse for (StatusCode, String) {
    fn into_response(self) -> HttpResponse {
        Response::builder()
            .status(self.0)
            .header(CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(
                Full::new(Bytes::from(self.1))
                    .map_err(|never| match never {})
                    .boxed(),
            )
            .expect("response builder should be infallible")
    }
}

impl IntoResponse for (StatusCode, &'static str) {
    fn into_response(self) -> HttpResponse {
        Response::builder()
            .status(self.0)
            .header(CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(
                Full::new(Bytes::from(self.1))
                    .map_err(|never| match never {})
                    .boxed(),
            )
            .expect("response builder should be infallible")
    }
}

impl IntoResponse for (StatusCode, Bytes) {
    fn into_response(self) -> HttpResponse {
        Response::builder()
            .status(self.0)
            .header(CONTENT_TYPE, "application/octet-stream")
            .body(Full::new(self.1).map_err(|never| match never {}).boxed())
            .expect("response builder should be infallible")
    }
}

// ── Json<T> response ──

impl<T: Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> HttpResponse {
        match serde_json::to_vec(&self.0) {
            Ok(bytes) => Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_TYPE, "application/json")
                .body(
                    Full::new(Bytes::from(bytes))
                        .map_err(|never| match never {})
                        .boxed(),
                )
                .expect("response builder should be infallible"),
            Err(e) => json_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("JSON serialization failed: {e}"),
            ),
        }
    }
}

impl<T: Serialize> IntoResponse for (StatusCode, Json<T>) {
    fn into_response(self) -> HttpResponse {
        match serde_json::to_vec(&(self.1).0) {
            Ok(bytes) => Response::builder()
                .status(self.0)
                .header(CONTENT_TYPE, "application/json")
                .body(
                    Full::new(Bytes::from(bytes))
                        .map_err(|never| match never {})
                        .boxed(),
                )
                .expect("response builder should be infallible"),
            Err(e) => json_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("JSON serialization failed: {e}"),
            ),
        }
    }
}

// ── RFC 7807 Problem Details ──

/// RFC 7807 Problem Details for HTTP APIs.
///
/// Provides a standardized error response format with `Content-Type: application/problem+json`.
///
/// # Example
///
/// ```rust,ignore
/// ProblemDetail::new(404, "Resource Not Found")
///     .with_detail("Todo with id 42 was not found")
///     .with_instance("/api/todos/42")
///     .with_extension("trace_id", "abc123")
/// ```
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProblemDetail {
    /// A URI reference identifying the problem type (default: "about:blank").
    #[serde(rename = "type")]
    pub type_uri: String,
    /// A short, human-readable summary of the problem type.
    pub title: String,
    /// The HTTP status code.
    pub status: u16,
    /// A human-readable explanation specific to this occurrence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// A URI reference identifying the specific occurrence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// Additional properties (trace_id, transition, axon, etc.).
    #[serde(skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub extensions: std::collections::HashMap<String, serde_json::Value>,
}

impl ProblemDetail {
    /// Create a new ProblemDetail with status and title.
    pub fn new(status: u16, title: impl Into<String>) -> Self {
        Self {
            type_uri: "about:blank".to_string(),
            title: title.into(),
            status,
            detail: None,
            instance: None,
            extensions: std::collections::HashMap::new(),
        }
    }

    /// Set the problem type URI.
    pub fn with_type_uri(mut self, uri: impl Into<String>) -> Self {
        self.type_uri = uri.into();
        self
    }

    /// Set the detail message.
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Set the instance URI.
    pub fn with_instance(mut self, instance: impl Into<String>) -> Self {
        self.instance = Some(instance.into());
        self
    }

    /// Add an extension property.
    pub fn with_extension(mut self, key: impl Into<String>, value: impl Into<serde_json::Value>) -> Self {
        self.extensions.insert(key.into(), value.into());
        self
    }
}

impl IntoResponse for ProblemDetail {
    fn into_response(self) -> HttpResponse {
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let body = serde_json::to_string(&self).unwrap_or_default();
        Response::builder()
            .status(status)
            .header(CONTENT_TYPE, "application/problem+json")
            .body(
                Full::new(Bytes::from(body))
                    .map_err(|never| match never {})
                    .boxed(),
            )
            .expect("response builder should be infallible")
    }
}

/// Trait for converting error types into RFC 7807 ProblemDetail.
///
/// Implement this trait on your error types to enable automatic
/// `Outcome::Fault` → `ProblemDetail` conversion.
#[allow(clippy::wrong_self_convention)]
pub trait IntoProblemDetail {
    fn into_problem_detail(&self) -> ProblemDetail;
}

/// Convert an `Outcome` to a response, using RFC 7807 for faults.
pub fn outcome_to_problem_response<Out, E>(outcome: Outcome<Out, E>) -> HttpResponse
where
    Out: IntoResponse,
    E: IntoProblemDetail,
{
    match outcome {
        Outcome::Next(output) => output.into_response(),
        Outcome::Fault(error) => error.into_problem_detail().into_response(),
        _ => "OK".into_response(),
    }
}

/// Convert an `Outcome` to an HTTP response with a safe default error handler.
///
/// In **debug builds** (`cfg(debug_assertions)`), the error's `Debug` output is
/// included in the response body to aid local development. In **release builds**,
/// only a generic "Internal server error" message is returned to prevent
/// information leakage (database details, file paths, internal types, etc.).
///
/// For custom error formatting, use [`outcome_to_response_with_error`] or
/// [`outcome_to_problem_response`] with [`IntoProblemDetail`].
pub fn outcome_to_response<Out, E>(outcome: Outcome<Out, E>) -> HttpResponse
where
    Out: IntoResponse,
    E: std::fmt::Debug,
{
    outcome_to_response_with_error(outcome, |error| {
        if cfg!(debug_assertions) {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Error: {:?}", error),
            )
                .into_response()
        } else {
            json_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal server error",
            )
        }
    })
}

pub fn outcome_to_response_with_error<Out, E, F>(
    outcome: Outcome<Out, E>,
    on_fault: F,
) -> HttpResponse
where
    Out: IntoResponse,
    F: FnOnce(&E) -> HttpResponse,
{
    match outcome {
        Outcome::Next(output) => output.into_response(),
        Outcome::Fault(error) => on_fault(&error),
        _ => "OK".into_response(),
    }
}

/// Convert an `Outcome<Out, E>` to an HTTP response, serializing `Out` as JSON.
///
/// Unlike [`outcome_to_response`] (which requires `Out: IntoResponse`), this function
/// only requires `Out: Serialize` and always produces `application/json` output.
///
/// This is the core function powering `get_json_out`, `post_typed_json_out`, etc.
/// The serialization format is decided at the route level, not in the Transition —
/// aligning with PHILOSOPHY.md §5 "Infrastructure as Boundary".
pub fn outcome_to_json_response<Out, E>(outcome: Outcome<Out, E>) -> HttpResponse
where
    Out: Serialize,
    E: std::fmt::Debug,
{
    match outcome {
        Outcome::Next(output) => Json(output).into_response(),
        Outcome::Fault(error) => {
            if cfg!(debug_assertions) {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Error: {:?}", error),
                )
                    .into_response()
            } else {
                json_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal server error",
                )
            }
        }
        _ => "OK".into_response(),
    }
}

/// Convert an `Outcome<Out, E>` to a JSON response, using RFC 7807 for faults.
///
/// Like [`outcome_to_json_response`], `Out` is auto-serialized to JSON.
/// On fault, the error is converted via [`IntoProblemDetail`].
pub fn outcome_to_json_problem_response<Out, E>(outcome: Outcome<Out, E>) -> HttpResponse
where
    Out: Serialize,
    E: IntoProblemDetail,
{
    match outcome {
        Outcome::Next(output) => Json(output).into_response(),
        Outcome::Fault(error) => error.into_problem_detail().into_response(),
        _ => "OK".into_response(),
    }
}

/// A wrapper for Askama templates that implements `IntoResponse`.
///
/// Renders the template to HTML and returns a `200 OK` response with
/// `text/html; charset=utf-8` content type. On render error, returns
/// `500 Internal Server Error` with a JSON error body.
///
/// # Example
///
/// ```rust,ignore
/// use askama::Template;
/// use ranvier_http::response::TemplateResponse;
///
/// #[derive(Template)]
/// #[template(path = "index.html")]
/// struct IndexPage { title: String }
///
/// let response = TemplateResponse(IndexPage { title: "Home".into() });
/// // response.into_response() → 200 OK, text/html
/// ```
#[cfg(feature = "askama")]
pub struct TemplateResponse<T: askama::Template>(pub T);

#[cfg(feature = "askama")]
impl<T: askama::Template> IntoResponse for TemplateResponse<T> {
    fn into_response(self) -> HttpResponse {
        match self.0.render() {
            Ok(html) => Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_TYPE, "text/html; charset=utf-8")
                .body(
                    Full::new(Bytes::from(html))
                        .map_err(|never| match never {})
                        .boxed(),
                )
                .expect("valid HTTP response construction"),
            Err(e) => json_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Template render error: {}", e),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ranvier_core::Outcome;

    #[test]
    fn string_into_response_sets_200_and_text_body() {
        let response = "hello".to_string().into_response();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn tuple_into_response_preserves_status_code() {
        let response = (StatusCode::CREATED, "created").into_response();
        assert_eq!(response.status(), StatusCode::CREATED);
    }

    #[test]
    fn outcome_fault_maps_to_internal_server_error() {
        let response = outcome_to_response::<String, &str>(Outcome::Fault("boom"));
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn json_error_response_sets_json_content_type() {
        let response = json_error_response(StatusCode::UNAUTHORIZED, "forbidden");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
    }

    #[test]
    fn problem_detail_new_sets_defaults() {
        let pd = ProblemDetail::new(404, "Not Found");
        assert_eq!(pd.status, 404);
        assert_eq!(pd.title, "Not Found");
        assert_eq!(pd.type_uri, "about:blank");
        assert!(pd.detail.is_none());
        assert!(pd.instance.is_none());
        assert!(pd.extensions.is_empty());
    }

    #[test]
    fn problem_detail_builder_methods() {
        let pd = ProblemDetail::new(400, "Bad Request")
            .with_type_uri("https://ranvier.studio/errors/validation")
            .with_detail("2 validation errors")
            .with_instance("/api/todos")
            .with_extension("trace_id", "abc123");
        assert_eq!(pd.type_uri, "https://ranvier.studio/errors/validation");
        assert_eq!(pd.detail.as_deref(), Some("2 validation errors"));
        assert_eq!(pd.instance.as_deref(), Some("/api/todos"));
        assert_eq!(pd.extensions.get("trace_id").unwrap(), "abc123");
    }

    #[test]
    fn problem_detail_into_response_sets_problem_json_content_type() {
        let pd = ProblemDetail::new(404, "Not Found");
        let response = pd.into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/problem+json")
        );
    }

    #[test]
    fn problem_detail_serialization_roundtrip() {
        let pd = ProblemDetail::new(500, "Internal Server Error")
            .with_detail("Something went wrong")
            .with_extension("transition", "GetUser");
        let json = serde_json::to_string(&pd).unwrap();
        let pd2: ProblemDetail = serde_json::from_str(&json).unwrap();
        assert_eq!(pd2.status, 500);
        assert_eq!(pd2.title, "Internal Server Error");
        assert_eq!(pd2.detail.as_deref(), Some("Something went wrong"));
    }

    #[test]
    fn outcome_to_problem_response_maps_fault_to_rfc7807() {
        struct MyError;
        impl IntoProblemDetail for MyError {
            fn into_problem_detail(&self) -> ProblemDetail {
                ProblemDetail::new(422, "Unprocessable Entity")
            }
        }
        let outcome: Outcome<String, MyError> = Outcome::Fault(MyError);
        let response = outcome_to_problem_response(outcome);
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    // ── Json<T> IntoResponse tests ──

    #[test]
    fn json_into_response_sets_json_content_type() {
        #[derive(serde::Serialize)]
        struct Payload { id: u32, name: String }

        let response = Json(Payload { id: 1, name: "test".into() }).into_response();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).and_then(|v| v.to_str().ok()),
            Some("application/json")
        );
    }

    #[test]
    fn status_code_json_into_response() {
        #[derive(serde::Serialize)]
        struct Created { id: u32 }

        let response = (StatusCode::CREATED, Json(Created { id: 42 })).into_response();
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).and_then(|v| v.to_str().ok()),
            Some("application/json")
        );
    }

    // ── outcome_to_json_response tests ──

    #[test]
    fn outcome_to_json_response_serializes_struct() {
        #[derive(serde::Serialize)]
        struct ApiResult { count: u32 }

        let outcome: Outcome<ApiResult, String> = Outcome::Next(ApiResult { count: 5 });
        let response = outcome_to_json_response(outcome);
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).and_then(|v| v.to_str().ok()),
            Some("application/json")
        );
    }

    #[test]
    fn outcome_to_json_response_fault_returns_500() {
        let outcome: Outcome<String, String> = Outcome::Fault("boom".to_string());
        let response = outcome_to_json_response(outcome);
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn outcome_to_json_problem_response_maps_fault() {
        #[derive(serde::Serialize)]
        struct Data { ok: bool }

        struct AppError;
        impl IntoProblemDetail for AppError {
            fn into_problem_detail(&self) -> ProblemDetail {
                ProblemDetail::new(422, "Validation Failed")
            }
        }

        let outcome: Outcome<Data, AppError> = Outcome::Fault(AppError);
        let response = outcome_to_json_problem_response(outcome);
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }
}
