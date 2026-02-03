//! # RanvierService - Tower Service Adapter
//!
//! Adapts Ranvier Axon execution to Tower's Service trait.
//! This allows Ranvier circuits to be used with any Tower-compatible infrastructure.
//!
//! ## Design (Discussion 190)
//!
//! > "ranvier-http is an adapter that converts Ranvier Axon into tower::Service"

use bytes::Bytes;
use http::{Request, Response};
use http_body_util::Full;
use ranvier_core::prelude::*;
use ranvier_runtime::Axon;
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tower::Service;

/// The foundational logic engine service.
/// Adapts HTTP requests to Axon executions.
#[derive(Clone)]
pub struct RanvierService<In, Out, E, F> {
    axon: Axon<In, Out, E>,
    /// Converts a Request into the Axon's input state and potentially populates the Bus.
    converter: F,
}

impl<In, Out, E, F> RanvierService<In, Out, E, F> {
    pub fn new(axon: Axon<In, Out, E>, converter: F) -> Self {
        Self { axon, converter }
    }
}

impl<B, In, Out, E, F> Service<Request<B>> for RanvierService<In, Out, E, F>
where
    B: Send + 'static,
    In: Send + Sync + 'static,
    Out: Send + Sync + 'static,
    E: Send + 'static + std::fmt::Debug,
    F: Fn(Request<B>, &mut Bus) -> In + Clone + Send + Sync + 'static,
{
    type Response = Response<Full<Bytes>>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        let axon = self.axon.clone();
        let converter = self.converter.clone();

        Box::pin(async move {
            let mut bus = Bus::new();

            // 1. Ingress Adapter: Request -> In + Bus
            let input = converter(req, &mut bus);

            // 2. Run Axon
            let _result = axon.execute(input, &mut bus).await;

            // 3. Egress Adapter: Outcome -> Response
            // TODO: Properly map Outcome to Response based on application needs
            let body_str = format!(
                "Ranvier Execution Result: {:?}",
                "result (Debug missing on Outcome?)"
            );

            let response = Response::new(Full::new(Bytes::from(body_str)));
            Ok(response)
        })
    }
}
