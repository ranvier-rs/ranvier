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
        let auth_ctx = match authenticate(&request, &self.secret) {
            Ok(context) => context,
            Err(()) => return std::future::ready(Err(unauthorized())),
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
#[allow(dead_code)]
pub struct AuthLayer {
    pub secret: String,
}

#[allow(dead_code)]
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
#[allow(dead_code)]
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
        // Move the instance whose readiness was polled, not an unready clone.
        let replacement = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, replacement);

        Box::pin(async move {
            let auth_ctx = match authenticate(&req, &secret) {
                Ok(context) => context,
                Err(()) => return Ok(unauthorized()),
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

// One policy for both layer implementations. Ambiguous credentials fail closed.
fn authenticate<B>(request: &Request<B>, secret: &str) -> Result<super::auth::AuthContext, ()> {
    let mut headers = request
        .headers()
        .get_all(http::header::AUTHORIZATION)
        .iter();
    let header = headers.next().ok_or(())?;
    if headers.next().is_some() {
        return Err(());
    }
    let token = header
        .to_str()
        .map_err(|_| ())?
        .strip_prefix("Bearer ")
        .ok_or(())?;
    if token.is_empty() || token.len() > 8192 || token.bytes().any(|b| b.is_ascii_whitespace()) {
        return Err(());
    }
    validate_jwt(token, secret).map_err(|_| ())
}

fn unauthorized<B: From<String>>() -> Response<B> {
    let mut response = Response::new(B::from("Unauthorized".to_owned()));
    *response.status_mut() = StatusCode::UNAUTHORIZED;
    response.headers_mut().insert(
        http::header::WWW_AUTHENTICATE,
        http::HeaderValue::from_static("Bearer"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::Infallible;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    struct ProtectedService {
        ready: bool,
        calls: Arc<AtomicUsize>,
    }

    impl Clone for ProtectedService {
        fn clone(&self) -> Self {
            Self {
                ready: false,
                calls: self.calls.clone(),
            }
        }
    }

    impl Service<Request<String>> for ProtectedService {
        type Response = Response<String>;
        type Error = Infallible;
        type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            self.ready = true;
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, request: Request<String>) -> Self::Future {
            assert!(self.ready, "call must use the instance polled ready");
            self.ready = false;
            let identity = request
                .extensions()
                .get::<super::super::auth::AuthContext>()
                .unwrap();
            assert_eq!(identity.user_id, "alice");
            self.calls.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Ok(Response::new(request.into_body())))
        }
    }

    fn token(subject: &str, expires: i64, secret: &str) -> String {
        jsonwebtoken::encode(
            &jsonwebtoken::Header::default(),
            &serde_json::json!({"sub": subject, "roles": ["user"], "exp": expires}),
            &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap()
    }

    async fn exercise<S>(mut service: S, calls: Arc<AtomicUsize>)
    where
        S: Service<Request<String>, Response = Response<String>, Error = Infallible>,
    {
        let now = chrono::Utc::now().timestamp();
        let valid = token("alice", now + 3600, "test-secret");
        let rejected = [
            vec![],
            vec!["Basic abc".to_owned()],
            vec!["Bearer ".to_owned()],
            vec!["Bearer malformed".to_owned()],
            vec![format!(
                "Bearer {}",
                token("alice", now + 3600, "wrong-secret")
            )],
            vec![format!(
                "Bearer {}",
                token("alice", now - 3600, "test-secret")
            )],
            vec![format!("Bearer {}", token("  ", now + 3600, "test-secret"))],
            vec![format!("Bearer {valid}"), format!("Bearer {valid}")],
            vec![format!("Bearer {}", "x".repeat(8193))],
        ];
        for headers in rejected {
            let mut request = Request::new("private-input".to_owned());
            for header in headers {
                request
                    .headers_mut()
                    .append(http::header::AUTHORIZATION, header.parse().unwrap());
            }
            std::future::poll_fn(|cx| service.poll_ready(cx))
                .await
                .unwrap();
            let response = service.call(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(response.body(), "Unauthorized");
            assert_eq!(response.headers()[http::header::WWW_AUTHENTICATE], "Bearer");
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        }
        for _ in 0..2 {
            let mut request = Request::new("public-result".to_owned());
            request.headers_mut().insert(
                http::header::AUTHORIZATION,
                format!("Bearer {valid}").parse().unwrap(),
            );
            std::future::poll_fn(|cx| service.poll_ready(cx))
                .await
                .unwrap();
            let response = service.call(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.body(), "public-result");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn high_level_layer_rejects_without_domain_effects() {
        let calls = Arc::new(AtomicUsize::new(0));
        let service = jwt_auth_layer("test-secret".into()).layer(ProtectedService {
            ready: false,
            calls: calls.clone(),
        });
        exercise(service, calls).await;
    }

    #[tokio::test]
    async fn manual_layer_preserves_readiness_and_rejects_without_effects() {
        let calls = Arc::new(AtomicUsize::new(0));
        let service = AuthLayer::new("test-secret".into()).layer(ProtectedService {
            ready: false,
            calls: calls.clone(),
        });
        exercise(service, calls).await;
    }
}
