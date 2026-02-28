use std::collections::HashMap;
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use bytes::Bytes;
use http::header::{AUTHORIZATION, HeaderName};
use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt, Full};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};
use ranvier_core::Bus;
use serde::{Deserialize, Serialize};
use tower::{Layer, Service};

type BoxFuture<T> = Pin<Box<dyn Future<Output = Result<T, Infallible>> + Send>>;

/// Auth policy for a protected request path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthPolicy {
    Required,
    Optional,
    None,
}

/// Source scheme of an authenticated subject.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AuthScheme {
    Bearer,
    ApiKey,
}

/// Auth context propagated through request extensions and Bus.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AuthContext {
    pub subject: String,
    pub roles: Vec<String>,
    pub scheme: AuthScheme,
}

impl AuthContext {
    pub fn new(subject: impl Into<String>, roles: Vec<String>, scheme: AuthScheme) -> Self {
        Self {
            subject: subject.into(),
            roles,
            scheme,
        }
    }

    pub fn has_role(&self, role: &str) -> bool {
        self.roles.iter().any(|candidate| candidate == role)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("missing credentials")]
    MissingCredentials,
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("forbidden")]
    Forbidden,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct BearerClaims {
    sub: String,
    #[serde(default)]
    roles: Vec<String>,
    exp: usize,
}

/// Tower layer for Bearer JWT authentication.
#[derive(Clone)]
pub struct BearerAuthLayer {
    policy: AuthPolicy,
    decoding_key: Arc<DecodingKey>,
    validation: Validation,
}

impl BearerAuthLayer {
    pub fn new_hs256(secret: impl AsRef<[u8]>) -> Self {
        Self {
            policy: AuthPolicy::Required,
            decoding_key: Arc::new(DecodingKey::from_secret(secret.as_ref())),
            validation: Validation::new(Algorithm::HS256),
        }
    }

    pub fn policy(mut self, policy: AuthPolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn required(self) -> Self {
        self.policy(AuthPolicy::Required)
    }

    pub fn optional(self) -> Self {
        self.policy(AuthPolicy::Optional)
    }

    pub fn none(self) -> Self {
        self.policy(AuthPolicy::None)
    }
}

impl<S> Layer<S> for BearerAuthLayer {
    type Service = BearerAuthService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        BearerAuthService {
            inner,
            policy: self.policy,
            decoding_key: self.decoding_key.clone(),
            validation: self.validation.clone(),
        }
    }
}

#[derive(Clone)]
pub struct BearerAuthService<S> {
    inner: S,
    policy: AuthPolicy,
    decoding_key: Arc<DecodingKey>,
    validation: Validation,
}

impl<S, B> Service<Request<B>> for BearerAuthService<S>
where
    S: Service<Request<B>, Response = Response<http_body_util::combinators::BoxBody<bytes::Bytes, std::convert::Infallible>>, Error = Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    B: Send + 'static,
{
    type Response = Response<http_body_util::combinators::BoxBody<bytes::Bytes, std::convert::Infallible>>;
    type Error = Infallible;
    type Future = BoxFuture<Self::Response>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        let mut inner = self.inner.clone();
        let policy = self.policy;
        let decoding_key = self.decoding_key.clone();
        let validation = self.validation.clone();

        Box::pin(async move {
            if policy == AuthPolicy::None {
                return inner.call(req).await;
            }

            let mut req = req;
            let token = extract_bearer_token(req.headers());
            match token {
                Some(token) => match validate_bearer_token(&token, &decoding_key, &validation) {
                    Ok(context) => {
                        req.extensions_mut().insert(context);
                    }
                    Err(_) => {
                        return Ok(auth_error_response(
                            StatusCode::UNAUTHORIZED,
                            "invalid_credentials",
                            "invalid bearer token",
                        ));
                    }
                },
                None => {
                    if policy == AuthPolicy::Required {
                        return Ok(auth_error_response(
                            StatusCode::UNAUTHORIZED,
                            "missing_credentials",
                            "authorization bearer token is required",
                        ));
                    }
                }
            }

            inner.call(req).await
        })
    }
}

/// Tower layer for API-key authentication.
#[derive(Clone)]
pub struct ApiKeyAuthLayer {
    policy: AuthPolicy,
    header_name: HeaderName,
    key_map: Arc<HashMap<String, AuthContext>>,
}

impl ApiKeyAuthLayer {
    pub fn new<I>(keys: I) -> Self
    where
        I: IntoIterator<Item = (String, AuthContext)>,
    {
        Self {
            policy: AuthPolicy::Required,
            header_name: HeaderName::from_static("x-api-key"),
            key_map: Arc::new(keys.into_iter().collect()),
        }
    }

    pub fn policy(mut self, policy: AuthPolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn required(self) -> Self {
        self.policy(AuthPolicy::Required)
    }

    pub fn optional(self) -> Self {
        self.policy(AuthPolicy::Optional)
    }

    pub fn none(self) -> Self {
        self.policy(AuthPolicy::None)
    }

    pub fn header_name(mut self, name: HeaderName) -> Self {
        self.header_name = name;
        self
    }
}

impl<S> Layer<S> for ApiKeyAuthLayer {
    type Service = ApiKeyAuthService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ApiKeyAuthService {
            inner,
            policy: self.policy,
            header_name: self.header_name.clone(),
            key_map: self.key_map.clone(),
        }
    }
}

#[derive(Clone)]
pub struct ApiKeyAuthService<S> {
    inner: S,
    policy: AuthPolicy,
    header_name: HeaderName,
    key_map: Arc<HashMap<String, AuthContext>>,
}

impl<S, B> Service<Request<B>> for ApiKeyAuthService<S>
where
    S: Service<Request<B>, Response = Response<http_body_util::combinators::BoxBody<bytes::Bytes, std::convert::Infallible>>, Error = Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    B: Send + 'static,
{
    type Response = Response<http_body_util::combinators::BoxBody<bytes::Bytes, std::convert::Infallible>>;
    type Error = Infallible;
    type Future = BoxFuture<Self::Response>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        let mut inner = self.inner.clone();
        let policy = self.policy;
        let header_name = self.header_name.clone();
        let key_map = self.key_map.clone();

        Box::pin(async move {
            if policy == AuthPolicy::None {
                return inner.call(req).await;
            }

            let mut req = req;
            let key = req
                .headers()
                .get(&header_name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);

            match key {
                Some(key_value) => {
                    if let Some(context) = key_map.get(&key_value) {
                        req.extensions_mut().insert(context.clone());
                    } else {
                        return Ok(auth_error_response(
                            StatusCode::UNAUTHORIZED,
                            "invalid_credentials",
                            "invalid api key",
                        ));
                    }
                }
                None => {
                    if policy == AuthPolicy::Required {
                        return Ok(auth_error_response(
                            StatusCode::UNAUTHORIZED,
                            "missing_credentials",
                            "api key is required",
                        ));
                    }
                }
            }

            inner.call(req).await
        })
    }
}

/// Role-based authorization guard.
#[derive(Clone)]
pub struct RequireRoleLayer {
    role: Arc<String>,
}

impl RequireRoleLayer {
    pub fn new(role: impl Into<String>) -> Self {
        Self {
            role: Arc::new(role.into()),
        }
    }
}

impl<S> Layer<S> for RequireRoleLayer {
    type Service = RequireRoleService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RequireRoleService {
            inner,
            role: self.role.clone(),
        }
    }
}

#[derive(Clone)]
pub struct RequireRoleService<S> {
    inner: S,
    role: Arc<String>,
}

impl<S, B> Service<Request<B>> for RequireRoleService<S>
where
    S: Service<Request<B>, Response = Response<http_body_util::combinators::BoxBody<bytes::Bytes, std::convert::Infallible>>, Error = Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    B: Send + 'static,
{
    type Response = Response<http_body_util::combinators::BoxBody<bytes::Bytes, std::convert::Infallible>>;
    type Error = Infallible;
    type Future = BoxFuture<Self::Response>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        let mut inner = self.inner.clone();
        let role = self.role.clone();

        Box::pin(async move {
            let context = req.extensions().get::<AuthContext>().cloned();
            match context {
                Some(context) if context.has_role(role.as_str()) => inner.call(req).await,
                Some(_) => Ok(auth_error_response(
                    StatusCode::FORBIDDEN,
                    "forbidden",
                    "required role is missing",
                )),
                None => Ok(auth_error_response(
                    StatusCode::UNAUTHORIZED,
                    "missing_credentials",
                    "authentication context is missing",
                )),
            }
        })
    }
}

/// Move authenticated request context into Bus so Transition code can read it explicitly.
pub fn inject_auth_context(parts: &http::request::Parts, bus: &mut Bus) {
    if let Some(ctx) = parts.extensions.get::<AuthContext>() {
        bus.insert(ctx.clone());
    }
}
pub fn auth_context<'a>(bus: &'a Bus) -> Option<&'a AuthContext> {
    bus.read::<AuthContext>()
}

fn extract_bearer_token(headers: &http::HeaderMap) -> Option<String> {
    let value = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let token = value.strip_prefix("Bearer ")?;
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}

fn validate_bearer_token(
    token: &str,
    decoding_key: &DecodingKey,
    validation: &Validation,
) -> Result<AuthContext, AuthError> {
    let decoded = decode::<BearerClaims>(token, decoding_key, validation)
        .map_err(|_| AuthError::InvalidCredentials)?;

    Ok(AuthContext::new(
        decoded.claims.sub,
        decoded.claims.roles,
        AuthScheme::Bearer,
    ))
}

fn auth_error_response(
    status: StatusCode,
    code: &'static str,
    message: &'static str,
) -> Response<http_body_util::combinators::BoxBody<bytes::Bytes, std::convert::Infallible>> {
    let payload = serde_json::json!({
        "error": code,
        "message": message,
    });

    Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(payload.to_string())).boxed())
        .expect("auth error response should be infallible")
}

pub mod prelude {
    pub use crate::{
        ApiKeyAuthLayer, AuthContext, AuthPolicy, AuthScheme, BearerAuthLayer, RequireRoleLayer,
        auth_context, inject_auth_context,
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::{Request, StatusCode};
    use http_body_util::{BodyExt, Full};
    use jsonwebtoken::{EncodingKey, Header, encode};
    use std::time::{SystemTime, UNIX_EPOCH};
    use tower::ServiceExt;

    fn ok_service() -> impl Service<
        Request<Full<Bytes>>,
        Response = Response<http_body_util::combinators::BoxBody<bytes::Bytes, std::convert::Infallible>>,
        Error = Infallible,
        Future = impl Future<Output = Result<Response<http_body_util::combinators::BoxBody<bytes::Bytes, std::convert::Infallible>>, Infallible>> + Send,
    > + Clone {
        tower::service_fn(|req: Request<Full<Bytes>>| async move {
            let who = req
                .extensions()
                .get::<AuthContext>()
                .map(|ctx| ctx.subject.clone())
                .unwrap_or_else(|| "anonymous".to_string());
            Ok::<_, Infallible>(
                Response::builder()
                    .status(StatusCode::OK)
                    .body(Full::new(Bytes::from(who)).boxed())
                    .expect("response build"),
            )
        })
    }

    fn make_bearer_token(secret: &str, subject: &str, roles: &[&str]) -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("epoch")
            .as_secs() as usize;

        let claims = BearerClaims {
            sub: subject.to_string(),
            roles: roles.iter().map(|role| role.to_string()).collect(),
            exp: now + 60 * 60,
        };

        encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .expect("token encode")
    }

    #[tokio::test]
    async fn bearer_required_rejects_missing_token_with_401() {
        let layer = BearerAuthLayer::new_hs256("secret").required();
        let service = layer.layer(ok_service());

        let response = service
            .oneshot(Request::new(Full::new(Bytes::new())))
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn bearer_optional_allows_missing_token() {
        let layer = BearerAuthLayer::new_hs256("secret").optional();
        let service = layer.layer(ok_service());

        let response = service
            .oneshot(Request::new(Full::new(Bytes::new())))
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.expect("body collect");
        assert_eq!(&body.to_bytes()[..], b"anonymous");
    }

    #[tokio::test]
    async fn bearer_valid_token_injects_auth_context() {
        let secret = "super-secret";
        let token = make_bearer_token(secret, "alice", &["admin"]);
        let layer = BearerAuthLayer::new_hs256(secret);
        let service = layer.layer(ok_service());

        let request = Request::builder()
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .body(Full::new(Bytes::new()))
            .expect("request build");

        let response = service.oneshot(request).await.expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.expect("body collect");
        assert_eq!(&body.to_bytes()[..], b"alice");
    }

    #[tokio::test]
    async fn api_key_required_rejects_missing_key_with_401() {
        let layer = ApiKeyAuthLayer::new(Vec::<(String, AuthContext)>::new()).required();
        let service = layer.layer(ok_service());

        let response = service
            .oneshot(Request::new(Full::new(Bytes::new())))
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn role_guard_rejects_authenticated_user_without_role_with_403() {
        let secret = "role-secret";
        let token = make_bearer_token(secret, "bob", &["user"]);

        let service = BearerAuthLayer::new_hs256(secret)
            .layer(RequireRoleLayer::new("admin").layer(ok_service()));

        let request = Request::builder()
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .body(Full::new(Bytes::new()))
            .expect("request build");

        let response = service.oneshot(request).await.expect("response");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn inject_auth_context_moves_extension_into_bus() {
        let context = AuthContext::new("carol", vec!["admin".to_string()], AuthScheme::ApiKey);
        let mut request = Request::new(Full::new(Bytes::new()));
        request.extensions_mut().insert(context.clone());

        let mut bus = Bus::new();
        inject_auth_context(&request.into_parts().0, &mut bus);

        assert_eq!(auth_context(&bus), Some(&context));
    }
}
