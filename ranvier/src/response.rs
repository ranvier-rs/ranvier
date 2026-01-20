use bytes::Bytes;
use http::StatusCode;
use http_body_util::Full;
use hyper::Response;
use serde::Serialize;

/// Create a text/plain response
pub fn text(body: impl Into<Bytes>) -> Response<Full<Bytes>> {
    Response::builder()
        .header("content-type", "text/plain; charset=utf-8")
        .body(Full::new(body.into()))
        .unwrap()
}

/// Create a text/html response
pub fn html(body: impl Into<Bytes>) -> Response<Full<Bytes>> {
    Response::builder()
        .header("content-type", "text/html; charset=utf-8")
        .body(Full::new(body.into()))
        .unwrap()
}

/// Create a 404 Not Found response
pub fn not_found() -> Response<Full<Bytes>> {
    let mut res = Response::new(Full::new(Bytes::from("Not Found")));
    *res.status_mut() = StatusCode::NOT_FOUND;
    res
}

/// Create a JSON response
///
/// Note: This is a simplified implementation. In a real framework,
/// you'd want better error handling than unwrap.
pub fn json<T: Serialize>(body: &T) -> Response<Full<Bytes>> {
    let json = serde_json::to_vec(body).expect("Failed to serialize JSON");
    Response::builder()
        .header("content-type", "application/json")
        .body(Full::from(json))
        .unwrap()
}
