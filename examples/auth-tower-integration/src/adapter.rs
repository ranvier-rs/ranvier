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

#[cfg(test)]
mod tests {
    use super::*;
    use tower::Layer;

    #[ranvier_macros::transition]
    async fn fail_private(
        _input: (),
        _res: &(),
        _bus: &mut Bus,
    ) -> Outcome<DomainResponse, AppError> {
        Outcome::Fault(AppError::Internal("private-backend-secret".into()))
    }

    #[tokio::test]
    async fn domain_fault_details_are_not_exposed() {
        let mut service = AxonService {
            pipeline: Axon::simple::<AppError>("private-fault").then(fail_private),
        };
        let mut request = Request::new(String::new());
        request.extensions_mut().insert(AuthContext {
            user_id: "alice".into(),
            roles: vec![],
        });
        std::future::poll_fn(|cx| service.poll_ready(cx))
            .await
            .unwrap();
        let response = service.call(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(response.body(), "Internal server error");
    }

    #[tokio::test]
    async fn missing_extensions_are_rejected_at_adapter_boundary() {
        let mut service = AxonService {
            pipeline: Axon::simple::<AppError>("missing-extension")
                .then(super::super::protected_handler),
        };
        std::future::poll_fn(|cx| service.poll_ready(cx))
            .await
            .unwrap();
        let response = service.call(Request::new(String::new())).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response.body(), "Unauthorized");
    }

    #[tokio::test]
    async fn concurrent_requests_have_isolated_verified_identity() {
        let pipeline =
            Axon::simple::<AppError>("isolated-identity").then(super::super::protected_handler);
        let mut service =
            super::super::jwt_auth_layer("test-secret".into()).layer(AxonService { pipeline });
        let mut futures = Vec::new();
        for subject in ["alice", "bob"] {
            let token =
                super::super::create_test_token(subject, vec!["user".into()], "test-secret")
                    .unwrap();
            let mut request = Request::new(String::new());
            request.headers_mut().insert(
                http::header::AUTHORIZATION,
                format!("Bearer {token}").parse().unwrap(),
            );
            // The layer must replace this unverified upstream identity.
            request.extensions_mut().insert(AuthContext {
                user_id: "forged".into(),
                roles: vec!["admin".into()],
            });
            std::future::poll_fn(|cx| service.poll_ready(cx))
                .await
                .unwrap();
            futures.push(service.call(request));
        }
        let bob = futures.pop().unwrap();
        let alice = futures.pop().unwrap();
        let (alice, bob) = tokio::join!(alice, bob);
        for (response, subject) in [(alice.unwrap(), "alice"), (bob.unwrap(), "bob")] {
            assert_eq!(response.status(), StatusCode::OK);
            let body: serde_json::Value = serde_json::from_str(response.body()).unwrap();
            assert_eq!(body["user_id"], subject);
            assert_eq!(body["roles"], serde_json::json!(["user"]));
        }
    }
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
