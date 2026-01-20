use http::{Request, Response, HeaderName, HeaderValue};
use std::task::{Context, Poll};
use tower::{Layer, Service};
use std::future::Future;
use std::pin::Pin;
use crate::step::Step;

/// A simple step that adds a fixed header to the response.
#[derive(Clone)]
pub struct SetHeaderLayer {
    key: HeaderName,
    val: HeaderValue,
}

impl SetHeaderLayer {
    pub fn new(key: HeaderName, val: HeaderValue) -> Self {
        Self { key, val }
    }
}

impl<S> Layer<S> for SetHeaderLayer {
    type Service = SetHeaderService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        SetHeaderService {
            inner,
            key: self.key.clone(),
            val: self.val.clone(),
        }
    }
}

impl<S> Step<S> for SetHeaderLayer {
    fn id(&self) -> &'static str {
        "SetHeader"
    }
}

#[derive(Clone)]
pub struct SetHeaderService<S> {
    inner: S,
    key: HeaderName,
    val: HeaderValue,
}

impl<S, ReqBody, ResBody> Service<Request<ReqBody>> for SetHeaderService<S>
where
    S: Service<Request<ReqBody>, Response = Response<ResBody>>,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        // Clone for the async block
        let fut = self.inner.call(req);
        let key = self.key.clone();
        let val = self.val.clone();

        Box::pin(async move {
            let mut response = fut.await?;
            response.headers_mut().insert(key, val);
            Ok(response)
        })
    }
}
