use crate::{IsolationPolicy, TenantId, TenantResolver};
use futures_util::future::BoxFuture;
use http::{Request, Response, StatusCode};
use std::task::{Context, Poll};
use tower::{Layer, Service};

/// A Tower layer that extracts `TenantId` from the incoming HTTP request,
/// validates the isolation policy, and injects it into the request extensions.
#[derive(Clone)]
pub struct TenantRouterLayer {
    resolver: TenantResolver,
    policy: IsolationPolicy,
}

impl TenantRouterLayer {
    pub fn new(resolver: TenantResolver, policy: IsolationPolicy) -> Self {
        Self { resolver, policy }
    }
}

impl<S> Layer<S> for TenantRouterLayer {
    type Service = TenantRouterService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        TenantRouterService {
            inner,
            resolver: self.resolver.clone(),
            policy: self.policy.clone(),
        }
    }
}

#[derive(Clone)]
pub struct TenantRouterService<S> {
    inner: S,
    resolver: TenantResolver,
    policy: IsolationPolicy,
}

impl<S, ReqBody, ResBody> Service<Request<ReqBody>> for TenantRouterService<S>
where
    S: Service<Request<ReqBody>, Response = Response<ResBody>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    ReqBody: Send + 'static,
    ResBody: Default + Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<ReqBody>) -> Self::Future {
        let mut inner = self.inner.clone();

        let tenant_id = match &self.resolver {
            TenantResolver::Header(name) => req
                .headers()
                .get(*name)
                .and_then(|val| val.to_str().ok())
                .map(TenantId::new),
            TenantResolver::Subdomain => req
                .headers()
                .get(http::header::HOST)
                .and_then(|val| val.to_str().ok())
                .and_then(|host| host.split('.').next())
                .map(TenantId::new),
            TenantResolver::PathPrefix => {
                let path = req.uri().path();
                let mut parts = path.split('/');
                parts.next(); // Skip empty pre-slash
                parts.next().map(TenantId::new) // Grab first path segment
            }
        };

        let resolved = match (tenant_id, &self.policy) {
            (Some(id), _) => Some(id),
            (None, IsolationPolicy::DefaultTenant(id)) => Some(id.clone()),
            (None, IsolationPolicy::Strict) => None,
        };

        match resolved {
            Some(id) => {
                req.extensions_mut().insert(id);
                Box::pin(async move { inner.call(req).await })
            }
            None => Box::pin(async move {
                let mut res = Response::new(ResBody::default());
                *res.status_mut() = StatusCode::BAD_REQUEST;
                Ok(res)
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::{Request, Response, StatusCode};
    use std::convert::Infallible;
    use tower::ServiceBuilder;

    async fn handle_request(req: Request<()>) -> Result<Response<()>, Infallible> {
        let mut response = Response::new(());
        if let Some(tenant) = req.extensions().get::<TenantId>() {
            *response.status_mut() = StatusCode::OK;
            response.extensions_mut().insert(tenant.clone());
        } else {
            *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
        }
        Ok(response)
    }

    #[tokio::test]
    async fn test_tenant_router_layer_header() {
        let layer = TenantRouterLayer::new(
            TenantResolver::Header("x-tenant-id"),
            IsolationPolicy::Strict,
        );

        let mut service = ServiceBuilder::new()
            .layer(layer)
            .service_fn(|req: Request<()>| async move { handle_request(req).await });

        let request = Request::builder()
            .header("x-tenant-id", "tenant-123")
            .body(())
            .unwrap();

        let response = service.call(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let extracted_tenant = response.extensions().get::<TenantId>().unwrap();
        assert_eq!(extracted_tenant.as_str(), "tenant-123");
    }

    #[tokio::test]
    async fn test_tenant_router_layer_strict_rejection() {
        let layer = TenantRouterLayer::new(
            TenantResolver::Header("x-tenant-id"),
            IsolationPolicy::Strict,
        );

        let mut service = ServiceBuilder::new()
            .layer(layer)
            .service_fn(|req: Request<()>| async move { handle_request(req).await });

        let request = Request::builder().body(()).unwrap();

        let response = service.call(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
