//! Tower-based authentication layer.
//!
//! This module demonstrates two approaches to Tower integration:
//! - **Option A (Low-level)**: Manual `Layer` + `Service` implementation (educational)
//! - **Option B (High-level)**: Using `tower-http::auth::AsyncRequireAuthorizationLayer` (recommended)

use super::auth::validate_jwt;
use http::{Request, Response, StatusCode};
use std::task::{Context, Poll};
use tower::{Layer, Service};
use tower_http::auth::{AsyncAuthorizeRequest, AsyncRequireAuthorizationLayer};

// ============================================================================
// Option B: High-level Tower-http integration (Recommended)
// ============================================================================

/// JWT authorizer for `tower-http::auth::AsyncRequireAuthorizationLayer`.
///
/// This is the **recommended** approach as it:
/// - Reduces boilerplate (no manual Service implementation)
/// - Leverages battle-tested `tower-http` middleware
/// - Still allows full control over validation logic
#[derive(Clone)]
pub struct JwtAuthorizer {
    pub secret: String,
}

impl<B> AsyncAuthorizeRequest<B> for JwtAuthorizer
where
    B: Send + 'static,
{
    type RequestBody = B;
    type ResponseBody = String;
    type Future = std::future::Ready<Result<Request<B>, Response<Self::ResponseBody>>>;

    fn authorize(&mut self, mut request: Request<B>) -> Self::Future {
        // Extract Authorization header
        let auth_header = match request
            .headers()
            .get("authorization")
            .and_then(|h| h.to_str().ok())
        {
            Some(h) => h,
            None => {
                return std::future::ready(Err(Response::builder()
                    .status(StatusCode::UNAUTHORIZED)
                    .body("Missing Authorization header".to_string())
                    .unwrap()));
            }
        };

        // Extract token from "Bearer <token>"
        let token = match auth_header.strip_prefix("Bearer ") {
            Some(t) => t,
            None => {
                return std::future::ready(Err(Response::builder()
                    .status(StatusCode::UNAUTHORIZED)
                    .body("Invalid Authorization format".to_string())
                    .unwrap()));
            }
        };

        // Validate JWT
        let auth_ctx = match validate_jwt(token, &self.secret) {
            Ok(ctx) => ctx,
            Err(e) => {
                return std::future::ready(Err(Response::builder()
                    .status(StatusCode::UNAUTHORIZED)
                    .body(format!("Invalid token: {}", e))
                    .unwrap()));
            }
        };

        // Store AuthContext in request extensions for downstream use
        request.extensions_mut().insert(auth_ctx);

        std::future::ready(Ok(request))
    }
}

// ============================================================================
// Option A: Low-level Layer + Service (Educational)
// ============================================================================

/// Custom authentication layer (low-level approach).
///
/// This is for educational purposes to show how Tower layers work internally.
/// For production, prefer Option B (`RequireAuthorizationLayer`).
#[derive(Clone)]
pub struct AuthLayer {
    pub secret: String,
}

impl AuthLayer {
    pub fn new(secret: String) -> Self {
        Self { secret }
    }
}

impl<S> Layer<S> for AuthLayer {
    type Service = AuthService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        AuthService {
            inner,
            secret: self.secret.clone(),
        }
    }
}

/// Authentication service that wraps an inner service.
#[derive(Clone)]
pub struct AuthService<S> {
    inner: S,
    secret: String,
}

impl<S, ReqBody, ResBody> Service<Request<ReqBody>> for AuthService<S>
where
    S: Service<Request<ReqBody>, Response = Response<ResBody>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    ReqBody: Send + 'static,
    ResBody: Default + From<String> + Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<ReqBody>) -> Self::Future {
        let secret = self.secret.clone();
        let mut inner = self.inner.clone();

        Box::pin(async move {
            // Extract Authorization header
            let auth_header = match req
                .headers()
                .get("authorization")
                .and_then(|h| h.to_str().ok())
            {
                Some(h) => h,
                None => {
                    let response = Response::builder()
                        .status(StatusCode::UNAUTHORIZED)
                        .body(ResBody::from("Missing Authorization header".to_string()))
                        .unwrap();
                    return Ok(response);
                }
            };

            // Extract token
            let token = match auth_header.strip_prefix("Bearer ") {
                Some(t) => t,
                None => {
                    let response = Response::builder()
                        .status(StatusCode::UNAUTHORIZED)
                        .body(ResBody::from("Invalid Authorization format".to_string()))
                        .unwrap();
                    return Ok(response);
                }
            };

            // Validate JWT
            let auth_ctx = match validate_jwt(token, &secret) {
                Ok(ctx) => ctx,
                Err(e) => {
                    let response = Response::builder()
                        .status(StatusCode::UNAUTHORIZED)
                        .body(ResBody::from(format!("Invalid token: {}", e)))
                        .unwrap();
                    return Ok(response);
                }
            };

            // Store AuthContext in request extensions
            req.extensions_mut().insert(auth_ctx);

            // Call inner service
            inner.call(req).await
        })
    }
}

// ============================================================================
// Helper function for creating JWT authorization layer
// ============================================================================

/// Create a Tower authorization layer using Option B (recommended).
pub fn jwt_auth_layer(secret: String) -> AsyncRequireAuthorizationLayer<JwtAuthorizer> {
    AsyncRequireAuthorizationLayer::new(JwtAuthorizer { secret })
}
