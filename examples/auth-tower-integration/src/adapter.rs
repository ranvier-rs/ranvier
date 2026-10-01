//! In-process Tower boundary for the local, unpersisted demonstration.
//! HTTP transport limits and production durability policy are separate concerns.

use super::{AppError, Response as DomainResponse, auth::AuthContext};
use http::{Request, Response, StatusCode};
use ranvier_core::{Bus, Outcome};
use ranvier_runtime::{Axon, ExecutionReport};
use std::{
    convert::Infallible,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tower::Service;

#[derive(Clone)]
pub(super) struct AxonService {
    pub(super) pipeline: Axon<(), DomainResponse, AppError, ()>,
}

fn response(status: StatusCode, body: &str) -> Response<String> {
    let mut response = Response::new(body.to_owned());
    *response.status_mut() = status;
    response
}

impl Service<Request<String>> for AxonService {
    type Response = Response<String>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // This local adapter has no queue or concurrency budget.
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, mut request: Request<String>) -> Self::Future {
        let pipeline = self.pipeline.clone();
        let identity = request.extensions_mut().remove::<AuthContext>();
        Box::pin(async move {
            let Some(identity) = identity else {
                return Ok(response(StatusCode::UNAUTHORIZED, "Unauthorized"));
            };
            let mut bus = Bus::new();
            bus.insert(identity);
            let report = pipeline.execute((), &(), &mut bus).await;
            if report
                .cleanup()
                .is_some_and(|cleanup| !cleanup.is_complete())
            {
                return Ok(response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Reconciliation required",
                ));
            }
            let ExecutionReport::Unpersisted { result, .. } = report else {
                return Ok(response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Reconciliation required",
                ));
            };
            Ok(match result {
                Outcome::Next(domain) => match StatusCode::from_u16(domain.status) {
                    Ok(status) => response(status, &domain.body),
                    Err(_) => response(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error"),
                },
                Outcome::Fault(AppError::Unauthenticated) => {
                    response(StatusCode::UNAUTHORIZED, "Unauthorized")
                }
                _ => response(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error"),
            })
        })
    }
}
