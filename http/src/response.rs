use bytes::Bytes;
use http::header::CONTENT_TYPE;
use http::{Response, StatusCode};
use http_body_util::Full;
use ranvier_core::Outcome;

pub type HttpResponse = Response<Full<Bytes>>;

pub trait IntoResponse {
    fn into_response(self) -> HttpResponse;
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
            .body(Full::new(Bytes::from(self)))
            .expect("response builder should be infallible")
    }
}

impl IntoResponse for &'static str {
    fn into_response(self) -> HttpResponse {
        Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(Full::new(Bytes::from(self)))
            .expect("response builder should be infallible")
    }
}

impl IntoResponse for Bytes {
    fn into_response(self) -> HttpResponse {
        Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "application/octet-stream")
            .body(Full::new(self))
            .expect("response builder should be infallible")
    }
}

impl IntoResponse for serde_json::Value {
    fn into_response(self) -> HttpResponse {
        Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "application/json")
            .body(Full::new(Bytes::from(self.to_string())))
            .expect("response builder should be infallible")
    }
}

impl IntoResponse for () {
    fn into_response(self) -> HttpResponse {
        Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(Full::new(Bytes::new()))
            .expect("response builder should be infallible")
    }
}

impl IntoResponse for (StatusCode, String) {
    fn into_response(self) -> HttpResponse {
        Response::builder()
            .status(self.0)
            .header(CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(Full::new(Bytes::from(self.1)))
            .expect("response builder should be infallible")
    }
}

impl IntoResponse for (StatusCode, &'static str) {
    fn into_response(self) -> HttpResponse {
        Response::builder()
            .status(self.0)
            .header(CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(Full::new(Bytes::from(self.1)))
            .expect("response builder should be infallible")
    }
}

impl IntoResponse for (StatusCode, Bytes) {
    fn into_response(self) -> HttpResponse {
        Response::builder()
            .status(self.0)
            .header(CONTENT_TYPE, "application/octet-stream")
            .body(Full::new(self.1))
            .expect("response builder should be infallible")
    }
}

pub fn outcome_to_response<Out, E>(outcome: Outcome<Out, E>) -> HttpResponse
where
    Out: IntoResponse,
    E: std::fmt::Debug,
{
    match outcome {
        Outcome::Next(output) => output.into_response(),
        Outcome::Fault(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Error: {:?}", error),
        )
            .into_response(),
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
}
