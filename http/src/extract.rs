use async_trait::async_trait;
use bytes::Bytes;
use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt, Full};
use http_body::Body;
use hyper::body::Incoming;
use serde::de::DeserializeOwned;
use std::collections::HashMap;

use crate::ingress::PathParams;

pub const DEFAULT_BODY_LIMIT: usize = 1024 * 1024;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ExtractError {
    #[error("request body exceeds limit {limit} bytes (actual: {actual})")]
    BodyTooLarge { limit: usize, actual: usize },
    #[error("failed to read request body: {0}")]
    BodyRead(String),
    #[error("invalid JSON body: {0}")]
    InvalidJson(String),
    #[error("invalid query string: {0}")]
    InvalidQuery(String),
    #[error("missing path params in request extensions")]
    MissingPathParams,
    #[error("invalid path params: {0}")]
    InvalidPath(String),
    #[error("failed to encode path params: {0}")]
    PathEncode(String),
}

impl ExtractError {
    pub fn status_code(&self) -> StatusCode {
        StatusCode::BAD_REQUEST
    }

    pub fn into_http_response(&self) -> Response<Full<Bytes>> {
        Response::builder()
            .status(self.status_code())
            .body(Full::new(Bytes::from(self.to_string())))
            .expect("extract error response builder should be infallible")
    }
}

#[async_trait]
pub trait FromRequest<B = Incoming>: Sized
where
    B: Body<Data = Bytes> + Send + Unpin + 'static,
    B::Error: std::fmt::Display + Send + Sync + 'static,
{
    async fn from_request(req: &mut Request<B>) -> Result<Self, ExtractError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Json<T>(pub T);

impl<T> Json<T> {
    pub fn into_inner(self) -> T {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query<T>(pub T);

impl<T> Query<T> {
    pub fn into_inner(self) -> T {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Path<T>(pub T);

impl<T> Path<T> {
    pub fn into_inner(self) -> T {
        self.0
    }
}

#[async_trait]
impl<T, B> FromRequest<B> for Json<T>
where
    T: DeserializeOwned + Send + 'static,
    B: Body<Data = Bytes> + Send + Unpin + 'static,
    B::Error: std::fmt::Display + Send + Sync + 'static,
{
    async fn from_request(req: &mut Request<B>) -> Result<Self, ExtractError> {
        let bytes = read_body_limited(req, DEFAULT_BODY_LIMIT).await?;
        let value = parse_json_bytes(&bytes)?;
        Ok(Json(value))
    }
}

#[async_trait]
impl<T, B> FromRequest<B> for Query<T>
where
    T: DeserializeOwned + Send + 'static,
    B: Body<Data = Bytes> + Send + Unpin + 'static,
    B::Error: std::fmt::Display + Send + Sync + 'static,
{
    async fn from_request(req: &mut Request<B>) -> Result<Self, ExtractError> {
        let value = parse_query_str(req.uri().query().unwrap_or(""))?;
        Ok(Query(value))
    }
}

#[async_trait]
impl<T, B> FromRequest<B> for Path<T>
where
    T: DeserializeOwned + Send + 'static,
    B: Body<Data = Bytes> + Send + Unpin + 'static,
    B::Error: std::fmt::Display + Send + Sync + 'static,
{
    async fn from_request(req: &mut Request<B>) -> Result<Self, ExtractError> {
        let params = req
            .extensions()
            .get::<PathParams>()
            .ok_or(ExtractError::MissingPathParams)?;
        let value = parse_path_map(params.as_map())?;
        Ok(Path(value))
    }
}

async fn read_body_limited<B>(req: &mut Request<B>, limit: usize) -> Result<Bytes, ExtractError>
where
    B: Body<Data = Bytes> + Send + Unpin + 'static,
    B::Error: std::fmt::Display + Send + Sync + 'static,
{
    let body = req
        .body_mut()
        .collect()
        .await
        .map_err(|error| ExtractError::BodyRead(error.to_string()))?
        .to_bytes();

    if body.len() > limit {
        return Err(ExtractError::BodyTooLarge {
            limit,
            actual: body.len(),
        });
    }

    Ok(body)
}

fn parse_json_bytes<T>(bytes: &[u8]) -> Result<T, ExtractError>
where
    T: DeserializeOwned,
{
    serde_json::from_slice(bytes).map_err(|error| ExtractError::InvalidJson(error.to_string()))
}

fn parse_query_str<T>(query: &str) -> Result<T, ExtractError>
where
    T: DeserializeOwned,
{
    serde_urlencoded::from_str(query).map_err(|error| ExtractError::InvalidQuery(error.to_string()))
}

fn parse_path_map<T>(params: &HashMap<String, String>) -> Result<T, ExtractError>
where
    T: DeserializeOwned,
{
    let encoded =
        serde_urlencoded::to_string(params).map_err(|error| ExtractError::PathEncode(error.to_string()))?;
    serde_urlencoded::from_str(&encoded).map_err(|error| ExtractError::InvalidPath(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct QueryPayload {
        page: u32,
        size: u32,
    }

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct PathPayload {
        id: u64,
        slug: String,
    }

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct JsonPayload {
        id: u32,
        name: String,
    }

    #[test]
    fn parse_query_payload() {
        let payload: QueryPayload = parse_query_str("page=2&size=50").expect("query parse");
        assert_eq!(payload.page, 2);
        assert_eq!(payload.size, 50);
    }

    #[test]
    fn parse_path_payload() {
        let mut map = HashMap::new();
        map.insert("id".to_string(), "42".to_string());
        map.insert("slug".to_string(), "order-created".to_string());
        let payload: PathPayload = parse_path_map(&map).expect("path parse");
        assert_eq!(payload.id, 42);
        assert_eq!(payload.slug, "order-created");
    }

    #[test]
    fn parse_json_payload() {
        let payload: JsonPayload =
            parse_json_bytes(br#"{"id":7,"name":"ranvier"}"#).expect("json parse");
        assert_eq!(payload.id, 7);
        assert_eq!(payload.name, "ranvier");
    }

    #[test]
    fn extract_error_maps_to_bad_request() {
        let error = ExtractError::InvalidQuery("bad input".to_string());
        assert_eq!(error.status_code(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn json_from_request_with_full_body() {
        let body = Full::new(Bytes::from_static(br#"{"id":9,"name":"node"}"#));
        let mut req = Request::builder()
            .uri("/orders")
            .body(body)
            .expect("request build");

        let Json(payload): Json<JsonPayload> = Json::from_request(&mut req).await.expect("extract");
        assert_eq!(payload.id, 9);
        assert_eq!(payload.name, "node");
    }

    #[tokio::test]
    async fn query_and_path_from_request_extensions() {
        let body = Full::new(Bytes::new());
        let mut req = Request::builder()
            .uri("/orders/42?page=3&size=10")
            .body(body)
            .expect("request build");

        let mut params = HashMap::new();
        params.insert("id".to_string(), "42".to_string());
        params.insert("slug".to_string(), "created".to_string());
        req.extensions_mut().insert(PathParams::new(params));

        let Query(query): Query<QueryPayload> = Query::from_request(&mut req).await.expect("query");
        let Path(path): Path<PathPayload> = Path::from_request(&mut req).await.expect("path");

        assert_eq!(query.page, 3);
        assert_eq!(query.size, 10);
        assert_eq!(path.id, 42);
        assert_eq!(path.slug, "created");
    }
}
