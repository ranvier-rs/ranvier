use crate::store::{Session, SessionStore};
use bytes::Bytes;
use http::{Request, Response, header};
use http_body_util::Full;
use ranvier_core::prelude::Bus;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tower::{Layer, Service};

pub const DEFAULT_COOKIE_NAME: &str = "ranvier.sid";

/// Tower Layer for session management.
#[derive(Clone)]
pub struct SessionLayer<S> {
    store: Arc<S>,
    cookie_name: String,
}

impl<S: SessionStore> SessionLayer<S> {
    pub fn new(store: S) -> Self {
        Self {
            store: Arc::new(store),
            cookie_name: DEFAULT_COOKIE_NAME.to_string(),
        }
    }

    pub fn with_cookie_name(mut self, name: impl Into<String>) -> Self {
        self.cookie_name = name.into();
        self
    }
}

/// Helper to inject the extracted Session into the Ranvier Bus.
/// Usage: `ingress.bus_injector(inject_session)`
pub fn inject_session<B>(req: &Request<B>, bus: &mut Bus) {
    if let Some(session) = req.extensions().get::<Session>() {
        bus.insert(session.clone());
    }
}

impl<S, InnerService> Layer<InnerService> for SessionLayer<S>
where
    S: SessionStore + Clone + Send + Sync + 'static,
{
    type Service = SessionService<S, InnerService>;

    fn layer(&self, inner: InnerService) -> Self::Service {
        SessionService {
            inner,
            store: self.store.clone(),
            cookie_name: self.cookie_name.clone(),
        }
    }
}

/// The actual Tower Service wrapping the inner service.
#[derive(Clone)]
pub struct SessionService<S, InnerService> {
    inner: InnerService,
    store: Arc<S>,
    cookie_name: String,
}

impl<S, InnerService, ReqBody> Service<Request<ReqBody>> for SessionService<S, InnerService>
where
    S: SessionStore + Send + Sync + 'static,
    InnerService: Service<Request<ReqBody>, Response = Response<Full<Bytes>>> + Clone + Send + 'static,
    InnerService::Future: Send + 'static,
    ReqBody: Send + 'static,
{
    type Response = InnerService::Response;
    type Error = InnerService::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<ReqBody>) -> Self::Future {
        let store = self.store.clone();
        let cookie_name = self.cookie_name.clone();
        let mut inner = self.inner.clone();

        Box::pin(async move {
            // 1. Extract session ID from cookie
            let mut session_id = None;
            if let Some(cookie_header) = req.headers().get(header::COOKIE) {
                if let Ok(cookie_str) = cookie_header.to_str() {
                    for part in cookie_str.split(';') {
                        let part = part.trim();
                        if part.starts_with(&format!("{}=", cookie_name)) {
                            session_id = Some(part[cookie_name.len() + 1..].to_string());
                            break;
                        }
                    }
                }
            }

            // 2. Load session or create new
            let session = match session_id {
                Some(id) => {
                    match store.load(&id).await {
                        Ok(Some(s)) => s,
                        _ => Session::new(),
                    }
                }
                None => Session::new(),
            };

            let original_session_id = session.id().await;
            
            // 3. Inject session into request extensions
            req.extensions_mut().insert(session.clone());

            // 4. Call inner service
            let mut response: Response<Full<Bytes>> = inner.call(req).await?;

            // 5. Check if session was modified/destroyed and act accordingly
            if session.is_destroyed().await {
                let _ = store.destroy(&original_session_id).await;
                // Emit deletion cookie
                let cookie = format!("{}=; Path=/; Expires=Thu, 01 Jan 1970 00:00:00 GMT", cookie_name);
                response.headers_mut().append(
                    header::SET_COOKIE,
                    http::HeaderValue::from_str(&cookie).unwrap(),
                );
            } else if session.is_modified().await {
                // Save mutated session
                let _ = store.save(&session).await;
                
                // If it's a new session or ID changed (e.g. regenerated), issue a cookie
                let current_id = session.id().await;
                // Even if not new, we might want to update expiry, but for now just basic "always set if modified"
                let cookie = format!("{}=path=/; HttpOnly; SameSite=Lax", current_id);
                // In a real implementation we'd probably add proper generic cookie builder here.
                let cookie_val = format!("{}={}; Path=/; HttpOnly; SameSite=Lax", cookie_name, current_id);
                
                response.headers_mut().append(
                    header::SET_COOKIE,
                    http::HeaderValue::from_str(&cookie_val).unwrap()
                );
            }

            Ok(response)
        })
    }
}
