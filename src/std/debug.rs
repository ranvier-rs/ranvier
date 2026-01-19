use crate::step::Step;
use std::fmt::Debug;
use std::task::{Context, Poll};
use tower::{Layer, Service};

/// A step that logs the request using `tracing`.
#[derive(Clone, Default)]
pub struct LogLayer {
    msg: String,
}

impl LogLayer {
    pub fn new(msg: impl Into<String>) -> Self {
        Self { msg: msg.into() }
    }
}

impl<S> Layer<S> for LogLayer {
    type Service = LogService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        LogService {
            inner,
            msg: self.msg.clone(),
        }
    }
}

impl<S> Step<S> for LogLayer {
    fn id(&self) -> &'static str {
        "Log"
    }
}

#[derive(Clone)]
pub struct LogService<S> {
    inner: S,
    msg: String,
}

impl<S, Request> Service<Request> for LogService<S>
where
    S: Service<Request>,
    Request: Debug,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        tracing::info!("{} - Request: {:?}", self.msg, req);
        self.inner.call(req)
    }
}
