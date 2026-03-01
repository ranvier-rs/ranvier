use async_trait::async_trait;
use bytes::Bytes;
use http::{Request, Response, StatusCode};
use http_body::Body;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use http::header::CONTENT_TYPE;

#[cfg(feature = "validation")]
use std::collections::BTreeMap;
#[cfg(feature = "validation")]
use validator::{Validate, ValidationErrors, ValidationErrorsKind};

use crate::ingress::PathParams;

#[cfg(feature = "multer")]
pub mod multipart;
#[cfg(feature = "multer")]
pub use multipart::Multipart;

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
    #[cfg(feature = "validation")]
    #[error("validation failed")]
    ValidationFailed(ValidationErrorBody),
    #[cfg(feature = "multer")]
    #[error("multipart parsing error: {0}")]
    MultipartError(String),
}

#[cfg(feature = "validation")]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ValidationErrorBody {
    pub error: &'static str,
    pub message: &'static str,
    pub fields: BTreeMap<String, Vec<String>>,
}

impl ExtractError {
    pub fn status_code(&self) -> StatusCode {
        #[cfg(feature = "validation")]
        {
            if matches!(self, Self::ValidationFailed(_)) {
                return StatusCode::UNPROCESSABLE_ENTITY;
            }
        }

        StatusCode::BAD_REQUEST
    }

    pub fn into_http_response(&self) -> Response<Full<Bytes>> {
        #[cfg(feature = "validation")]
        if let Self::ValidationFailed(body) = self {
            let payload = serde_json::to_vec(body).unwrap_or_else(|_| {
                br#"{"error":"validation_failed","message":"request validation failed"}"#.to_vec()
            });
            return Response::builder()
                .status(self.status_code())
                .header(http::header::CONTENT_TYPE, "application/json")
                .body(Full::new(Bytes::from(payload)))
                .expect("validation response builder should be infallible");
        }

        Response::builder()
            .status(self.status_code())
            .body(Full::new(Bytes::from(self.to_string())))
            .expect("extract error response builder should be infallible")
    }
}

/// Raw HTTP request body bytes injected into the Bus for body-aware routes.
///
/// Populated automatically when using `.post_body()`, `.put_body()`, or `.patch_body()`.
/// Access inside a transition via `bus.read::<HttpRequestBody>()`.
///
/// # Example
///
/// ```rust,ignore
/// use ranvier_http::prelude::*;
///
/// // In a transition:
/// let body_bytes = bus.read::<HttpRequestBody>()
///     .map(|b| b.as_bytes())
///     .unwrap_or_default();
/// ```
#[derive(Debug, Clone)]
pub struct HttpRequestBody(pub Bytes);

impl HttpRequestBody {
    /// Create a new HttpRequestBody from raw bytes.
    pub fn new(bytes: Bytes) -> Self {
        Self(bytes)
    }

    /// Access the raw bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Parse the body as JSON.
    pub fn parse_json<T: serde::de::DeserializeOwned>(&self) -> Result<T, ExtractError> {
        serde_json::from_slice(&self.0).map_err(|e| ExtractError::InvalidJson(e.to_string()))
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
#[cfg(not(feature = "validation"))]
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
#[cfg(feature = "validation")]
impl<T, B> FromRequest<B> for Json<T>
where
    T: DeserializeOwned + Send + Validate + 'static,
    B: Body<Data = Bytes> + Send + Unpin + 'static,
    B::Error: std::fmt::Display + Send + Sync + 'static,
{
    async fn from_request(req: &mut Request<B>) -> Result<Self, ExtractError> {
        let bytes = read_body_limited(req, DEFAULT_BODY_LIMIT).await?;
        let value = parse_json_bytes::<T>(&bytes)?;

        validate_payload(&value)?;
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

/// A multipart/form-data extractor.
pub struct Multipart {
    inner: multer::Multipart<'static>,
}

impl Multipart {
    /// Yields the next field from the multipart stream.
    pub async fn next_field(&mut self) -> Result<Option<MultipartField>, ExtractError> {
        self.inner
            .next_field()
            .await
            .map(|opt| opt.map(MultipartField))
            .map_err(|e| ExtractError::Multipart(e.to_string()))
    }
}

/// A field within a multipart/form-data stream.
pub struct MultipartField(multer::Field<'static>);

impl MultipartField {
    /// The name of the field.
    pub fn name(&self) -> Option<&str> {
        self.0.name()
    }

    /// The filename of the field.
    pub fn file_name(&self) -> Option<&str> {
        self.0.file_name()
    }

    /// The content-type of the field.
    pub fn content_type(&self) -> Option<&str> {
        self.0.content_type().map(|c| c.as_ref())
    }

    /// Read the field content as bytes.
    pub async fn bytes(self) -> Result<Bytes, ExtractError> {
        self.0
            .bytes()
            .await
            .map_err(|e| ExtractError::Multipart(e.to_string()))
    }

    /// Read the field content as text.
    pub async fn text(self) -> Result<String, ExtractError> {
        self.0
            .text()
            .await
            .map_err(|e| ExtractError::Multipart(e.to_string()))
    }
}

#[async_trait]
impl<B> FromRequest<B> for Multipart
where
    B: Body<Data = Bytes> + Send + Unpin + 'static,
    B::Error: std::fmt::Display + Send + Sync + 'static,
{
    async fn from_request(req: &mut Request<B>) -> Result<Self, ExtractError> {
        let content_type = req
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|ct| ct.to_str().ok())
            .ok_or(ExtractError::MissingContentType)?;

        let boundary = multer::parse_boundary(content_type).map_err(|_| ExtractError::InvalidContentType)?;

        // Collect the body bytes to avoid lifetime issues with req
        let body_bytes = BodyExt::collect(req.body_mut())
            .await
            .map_err(|e| ExtractError::BodyRead(e.to_string()))?
            .to_bytes();

        let stream = futures_util::stream::once(async move { Ok::<Bytes, std::io::Error>(body_bytes) });

        let multipart = multer::Multipart::new(stream, boundary);
        Ok(Multipart { inner: multipart })
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
    let encoded = serde_urlencoded::to_string(params)
        .map_err(|error| ExtractError::PathEncode(error.to_string()))?;
    serde_urlencoded::from_str(&encoded)
        .map_err(|error| ExtractError::InvalidPath(error.to_string()))
}

#[cfg(feature = "validation")]
fn validate_payload<T>(value: &T) -> Result<(), ExtractError>
where
    T: Validate,
{
    value
        .validate()
        .map_err(|errors| ExtractError::ValidationFailed(validation_error_body(&errors)))
}

#[cfg(feature = "validation")]
fn validation_error_body(errors: &ValidationErrors) -> ValidationErrorBody {
    let mut fields = BTreeMap::new();
    collect_validation_errors("", errors, &mut fields);

    ValidationErrorBody {
        error: "validation_failed",
        message: "request validation failed",
        fields,
    }
}

#[cfg(feature = "validation")]
fn collect_validation_errors(
    prefix: &str,
    errors: &ValidationErrors,
    fields: &mut BTreeMap<String, Vec<String>>,
) {
    for (field, kind) in errors.errors() {
        let field_path = if prefix.is_empty() {
            field.to_string()
        } else {
            format!("{prefix}.{field}")
        };

        match kind {
            ValidationErrorsKind::Field(failures) => {
                let entry = fields.entry(field_path).or_default();
                for failure in failures {
                    let detail = if let Some(message) = failure.message.as_ref() {
                        format!("{}: {}", failure.code, message)
                    } else {
                        failure.code.to_string()
                    };
                    entry.push(detail);
                }
            }
            ValidationErrorsKind::Struct(nested) => {
                collect_validation_errors(&field_path, nested, fields);
            }
            ValidationErrorsKind::List(items) => {
                for (index, nested) in items {
                    let list_path = format!("{field_path}[{index}]");
                    collect_validation_errors(&list_path, nested, fields);
                }
            }
        }
    }
}

use futures_util::StreamExt;

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    #[cfg(feature = "validation")]
    use validator::{Validate, ValidationErrors};

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
    #[cfg_attr(feature = "validation", derive(Validate))]
    struct JsonPayload {
        id: u32,
        name: String,
    }

    #[cfg(feature = "validation")]
    #[derive(Debug, Deserialize, Validate)]
    struct ValidatedPayload {
        #[validate(length(min = 3, message = "name too short"))]
        name: String,
        #[validate(range(min = 1, message = "age must be >= 1"))]
        age: u8,
    }

    #[cfg(feature = "validation")]
    #[derive(Debug, Deserialize, Validate)]
    #[validate(schema(function = "validate_password_confirmation"))]
    struct SignupPayload {
        #[validate(email(message = "email format invalid"))]
        email: String,
        password: String,
        confirm_password: String,
    }

    #[cfg(feature = "validation")]
    #[derive(Debug, Deserialize)]
    struct ManualValidatedPayload {
        token: String,
    }

    #[cfg(feature = "validation")]
    fn validate_password_confirmation(
        payload: &SignupPayload,
    ) -> Result<(), validator::ValidationError> {
        if payload.password != payload.confirm_password {
            return Err(validator::ValidationError::new("password_mismatch"));
        }
        Ok(())
    }

    #[cfg(feature = "validation")]
    impl Validate for ManualValidatedPayload {
        fn validate(&self) -> Result<(), ValidationErrors> {
            let mut errors = ValidationErrors::new();
            if !self.token.starts_with("tok_") {
                let mut error = validator::ValidationError::new("token_prefix");
                error.message = Some("token must start with tok_".into());
                errors.add("token", error);
            }

            if errors.errors().is_empty() {
                Ok(())
            } else {
                Err(errors)
            }
        }
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

    #[cfg(feature = "validation")]
    #[tokio::test]
    async fn json_validation_rejects_invalid_payload_with_422() {
        let body = Full::new(Bytes::from_static(br#"{"name":"ab","age":0}"#));
        let mut req = Request::builder()
            .uri("/users")
            .body(body)
            .expect("request build");

        let error = Json::<ValidatedPayload>::from_request(&mut req)
            .await
            .expect_err("payload should fail validation");

        assert_eq!(error.status_code(), StatusCode::UNPROCESSABLE_ENTITY);

        let response = error.into_http_response();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            response.headers().get(http::header::CONTENT_TYPE),
            Some(&http::HeaderValue::from_static("application/json"))
        );

        let body = response.into_body().collect().await.expect("collect body");
        let json: serde_json::Value =
            serde_json::from_slice(&body.to_bytes()).expect("validation json body");
        assert_eq!(json["error"], "validation_failed");
        assert!(
            json["fields"]["name"][0]
                .as_str()
                .expect("name message")
                .contains("name too short")
        );
        assert!(
            json["fields"]["age"][0]
                .as_str()
                .expect("age message")
                .contains("age must be >= 1")
        );
    }

    #[cfg(feature = "validation")]
    #[tokio::test]
    async fn json_validation_supports_schema_level_rules() {
        let body = Full::new(Bytes::from_static(
            br#"{"email":"user@example.com","password":"secret123","confirm_password":"different"}"#,
        ));
        let mut req = Request::builder()
            .uri("/signup")
            .body(body)
            .expect("request build");

        let error = Json::<SignupPayload>::from_request(&mut req)
            .await
            .expect_err("schema validation should fail");
        assert_eq!(error.status_code(), StatusCode::UNPROCESSABLE_ENTITY);

        let response = error.into_http_response();
        let body = response.into_body().collect().await.expect("collect body");
        let json: serde_json::Value =
            serde_json::from_slice(&body.to_bytes()).expect("validation json body");

        assert_eq!(json["fields"]["__all__"][0], "password_mismatch");
    }

    #[cfg(feature = "validation")]
    #[tokio::test]
    async fn json_validation_accepts_valid_payload() {
        let body = Full::new(Bytes::from_static(br#"{"name":"valid-name","age":20}"#));
        let mut req = Request::builder()
            .uri("/users")
            .body(body)
            .expect("request build");

        let Json(payload): Json<ValidatedPayload> = Json::from_request(&mut req)
            .await
            .expect("validation should pass");
        assert_eq!(payload.name, "valid-name");
        assert_eq!(payload.age, 20);
    }

    #[cfg(feature = "validation")]
    #[tokio::test]
    async fn json_validation_supports_manual_validate_impl_hooks() {
        let body = Full::new(Bytes::from_static(br#"{"token":"invalid"}"#));
        let mut req = Request::builder()
            .uri("/tokens")
            .body(body)
            .expect("request build");

        let error = Json::<ManualValidatedPayload>::from_request(&mut req)
            .await
            .expect_err("manual validation should fail");
        assert_eq!(error.status_code(), StatusCode::UNPROCESSABLE_ENTITY);

        let response = error.into_http_response();
        let body = response.into_body().collect().await.expect("collect body");
        let json: serde_json::Value =
            serde_json::from_slice(&body.to_bytes()).expect("validation json body");

        assert_eq!(
            json["fields"]["token"][0],
            "token_prefix: token must start with tok_"
        );
    }

    #[tokio::test]
    async fn extract_multipart_fields() {
        let boundary = "boundary";
        let body_content = format!(
            "--{boundary}\r\n\
             Content-Disposition: form-data; name=\"field1\"\r\n\
             \r\n\
             value1\r\n\
             --{boundary}\r\n\
             Content-Disposition: form-data; name=\"file\"; filename=\"test.txt\"\r\n\
             Content-Type: text/plain\r\n\
             \r\n\
             file content\r\n\
             --{boundary}--\r\n"
        );

        let mut req = Request::builder()
            .header(CONTENT_TYPE, format!("multipart/form-data; boundary={boundary}"))
            .body(Full::new(Bytes::from(body_content)))
            .expect("request build");

        let mut multipart = Multipart::from_request(&mut req).await.expect("extract");

        let field1 = multipart.next_field().await.expect("field1").unwrap();
        assert_eq!(field1.name(), Some("field1"));
        assert_eq!(field1.text().await.expect("text"), "value1");

        let file_field = multipart.next_field().await.expect("file").unwrap();
        assert_eq!(file_field.name(), Some("file"));
        assert_eq!(file_field.file_name(), Some("test.txt"));
        assert_eq!(file_field.content_type(), Some("text/plain"));
        assert_eq!(file_field.text().await.expect("text"), "file content");

        assert!(multipart.next_field().await.expect("none").is_none());
    }
}
