use bytes::Bytes;
use http::header::CONTENT_TYPE;
use http::{Response, StatusCode};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use ranvier_core::Outcome;
use std::convert::Infallible;

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

pub fn outcome_to_response<Out, E>(outcome: Outcome<Out, E>) -> HttpResponse
where
    Out: IntoResponse,
    E: std::fmt::Debug,
{
    outcome_to_response_with_error(outcome, |error| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Error: {:?}", error),
        )
            .into_response()
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
}
