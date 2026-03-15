use async_trait::async_trait;
use bytes::Bytes;
use http::{Request, Response, StatusCode};
use http_body::Body;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use serde::de::DeserializeOwned;
use std::collections::HashMap;

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
    #[error("missing header: {0}")]
    MissingHeader(String),
    #[error("invalid header value: {0}")]
    InvalidHeader(String),
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

/// Extract a single HTTP header value as a string.
///
/// # Example
///
/// ```rust,ignore
/// use ranvier_http::extract::Header;
///
/// let Header(auth) = Header::from_name("authorization", &mut req).await?;
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header(pub String);

impl Header {
    /// Extract a header by name from a request (non-trait convenience).
    pub fn from_parts(name: &str, parts: &http::request::Parts) -> Result<Self, ExtractError> {
        let value = parts
            .headers
            .get(name)
            .ok_or_else(|| ExtractError::MissingHeader(name.to_string()))?;
        let s = value
            .to_str()
            .map_err(|e| ExtractError::InvalidHeader(e.to_string()))?;
        Ok(Header(s.to_string()))
    }

    /// Get the inner string value.
    pub fn into_inner(self) -> String {
        self.0
    }
}

/// Extract all cookies from the `Cookie` header as key-value pairs.
///
/// # Example
///
/// ```rust,ignore
/// use ranvier_http::extract::CookieJar;
///
/// let jar = CookieJar::from_parts(&parts);
/// if let Some(session) = jar.get("session_id") {
///     // ...
/// }
/// ```
#[derive(Debug, Clone, Default)]
pub struct CookieJar {
    cookies: HashMap<String, String>,
}

/// Validate a cookie name against the HTTP token grammar (RFC 7230 §3.2.6).
///
/// token = 1*tchar
/// tchar = "!" / "#" / "$" / "%" / "&" / "'" / "*" / "+" / "-" / "." /
///         DIGIT / ALPHA / "^" / "_" / "`" / "|" / "~"
fn is_valid_cookie_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|b| matches!(b,
            b'!' | b'#' | b'$' | b'%' | b'&' | b'\'' | b'*' | b'+' | b'-' | b'.' |
            b'0'..=b'9' | b'A'..=b'Z' | b'^' | b'_' | b'`' | b'a'..=b'z' | b'|' | b'~'
        ))
}

/// Strip surrounding double-quotes from a cookie value (RFC 6265 §4.1.1).
fn unquote_cookie_value(value: &str) -> &str {
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

/// Percent-decode a cookie value (e.g., `hello%20world` → `hello world`).
fn percent_decode_cookie(input: &str) -> String {
    let mut result = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2])) {
                result.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        result.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&result).into_owned()
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

impl CookieJar {
    /// Parse cookies from request parts with RFC 6265 validation.
    ///
    /// Cookie names are validated against the HTTP token grammar (RFC 7230 §3.2.6).
    /// Invalid names are silently skipped with a `tracing::warn!` log entry.
    /// Cookie values are percent-decoded and unquoted.
    pub fn from_parts(parts: &http::request::Parts) -> Self {
        let mut cookies = HashMap::new();
        if let Some(header) = parts.headers.get(http::header::COOKIE) {
            if let Ok(value) = header.to_str() {
                for pair in value.split(';') {
                    let pair = pair.trim();
                    if let Some((key, val)) = pair.split_once('=') {
                        let name = key.trim();
                        if !is_valid_cookie_name(name) {
                            tracing::warn!(
                                cookie_name = name,
                                "skipping cookie with invalid name"
                            );
                            continue;
                        }
                        let val = unquote_cookie_value(val.trim());
                        cookies.insert(name.to_string(), percent_decode_cookie(val));
                    }
                }
            }
        }
        CookieJar { cookies }
    }

    /// Get a cookie value by name.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.cookies.get(name).map(|s| s.as_str())
    }

    /// Check if a cookie exists.
    pub fn contains(&self, name: &str) -> bool {
        self.cookies.contains_key(name)
    }

    /// Iterate over all cookies.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.cookies.iter().map(|(k, v)| (k.as_str(), v.as_str()))
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

    // ── CookieJar tests ──

    fn make_parts_with_cookie(cookie_value: &str) -> http::request::Parts {
        let (parts, _) = Request::builder()
            .header(http::header::COOKIE, cookie_value)
            .body(())
            .expect("request build")
            .into_parts();
        parts
    }

    #[test]
    fn cookiejar_parses_standard_cookies() {
        let parts = make_parts_with_cookie("session=abc123; lang=en");
        let jar = CookieJar::from_parts(&parts);
        assert_eq!(jar.get("session"), Some("abc123"));
        assert_eq!(jar.get("lang"), Some("en"));
    }

    #[test]
    fn cookiejar_skips_invalid_names() {
        // Spaces and commas are not valid token characters
        let parts = make_parts_with_cookie("good=yes; bad name=no; also,bad=no; ok=fine");
        let jar = CookieJar::from_parts(&parts);
        assert_eq!(jar.get("good"), Some("yes"));
        assert_eq!(jar.get("ok"), Some("fine"));
        assert!(jar.get("bad name").is_none());
        assert!(jar.get("also,bad").is_none());
    }

    #[test]
    fn cookiejar_unquotes_values() {
        let parts = make_parts_with_cookie("token=\"quoted_value\"");
        let jar = CookieJar::from_parts(&parts);
        assert_eq!(jar.get("token"), Some("quoted_value"));
    }

    #[test]
    fn cookiejar_percent_decodes_values() {
        let parts = make_parts_with_cookie("msg=hello%20world; path=%2Fapi%2Fv1");
        let jar = CookieJar::from_parts(&parts);
        assert_eq!(jar.get("msg"), Some("hello world"));
        assert_eq!(jar.get("path"), Some("/api/v1"));
    }

    #[test]
    fn cookiejar_handles_empty_header() {
        let parts = make_parts_with_cookie("");
        let jar = CookieJar::from_parts(&parts);
        assert!(jar.get("anything").is_none());
    }

    #[test]
    fn cookiejar_name_validation() {
        assert!(is_valid_cookie_name("session_id"));
        assert!(is_valid_cookie_name("__Host-token"));
        assert!(!is_valid_cookie_name(""));
        assert!(!is_valid_cookie_name("bad name"));
        assert!(!is_valid_cookie_name("bad,name"));
        assert!(!is_valid_cookie_name("bad(name)"));
    }
}
