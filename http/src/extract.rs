use async_trait::async_trait;
use bytes::Bytes;
use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt, Full};
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
pub trait FromRequest: Sized {
    async fn from_request(req: &mut Request<Incoming>) -> Result<Self, ExtractError>;
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
impl<T> FromRequest for Json<T>
where
    T: DeserializeOwned + Send + 'static,
{
    async fn from_request(req: &mut Request<Incoming>) -> Result<Self, ExtractError> {
        let bytes = read_body_limited(req, DEFAULT_BODY_LIMIT).await?;
        let value = parse_json_bytes(&bytes)?;
        Ok(Json(value))
    }
}

#[async_trait]
impl<T> FromRequest for Query<T>
where
    T: DeserializeOwned + Send + 'static,
{
    async fn from_request(req: &mut Request<Incoming>) -> Result<Self, ExtractError> {
        let value = parse_query_str(req.uri().query().unwrap_or(""))?;
        Ok(Query(value))
    }
}

#[async_trait]
impl<T> FromRequest for Path<T>
where
    T: DeserializeOwned + Send + 'static,
{
    async fn from_request(req: &mut Request<Incoming>) -> Result<Self, ExtractError> {
        let params = req
            .extensions()
            .get::<PathParams>()
            .ok_or(ExtractError::MissingPathParams)?;
        let value = parse_path_map(params.as_map())?;
        Ok(Path(value))
    }
}

async fn read_body_limited(
    req: &mut Request<Incoming>,
    limit: usize,
) -> Result<Bytes, ExtractError> {
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
}
