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
    use ranvier_runtime::{
        CompletionConfirmation, EffectBoundary, PersistenceOperation, SagaRollbackOutcome,
        SagaRollbackReport, SagaRollbackStep,
    };
    use tower::Layer;

    fn next() -> Outcome<DomainResponse, AppError> {
        Outcome::Next(DomainResponse {
            status: 200,
            body: "private-domain-result".into(),
        })
    }

    #[test]
    fn local_policy_rejects_every_persisted_report_variant() {
        let reports = [
            ExecutionReport::NotRequested {
                result: next(),
                cleanup: None,
            },
            ExecutionReport::Recorded {
                result: next(),
                completion: CompletionConfirmation::Confirmed,
                cleanup: None,
            },
            ExecutionReport::Recorded {
                result: next(),
                completion: CompletionConfirmation::NotRequested,
                cleanup: None,
            },
            ExecutionReport::Unconfirmed {
                result: next(),
                operation: PersistenceOperation::Result,
                effect: EffectBoundary::MayHaveRun,
                cleanup: None,
            },
            ExecutionReport::Unconfirmed {
                result: Outcome::Fault(AppError::Internal("private-failure".into())),
                operation: PersistenceOperation::Complete,
                effect: EffectBoundary::MayHaveRun,
                cleanup: None,
            },
            ExecutionReport::BlockedBeforeEffect {
                operation: PersistenceOperation::Intent,
                effect: EffectBoundary::NotInvoked,
                cleanup: None,
            },
            ExecutionReport::BlockedBeforeEffect {
                operation: PersistenceOperation::Intent,
                effect: EffectBoundary::MayHaveRun,
                cleanup: None,
            },
        ];
        for report in reports {
            let response = map_local_report(report);
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(response.body(), "Reconciliation required");
            assert!(!response.headers().contains_key(http::header::RETRY_AFTER));
        }
    }

    #[test]
    fn cleanup_must_finish_and_complete_every_hook_before_success() {
        for finished in [false, true] {
            for outcome in [
                SagaRollbackOutcome::InProgress,
                SagaRollbackOutcome::Panicked,
                SagaRollbackOutcome::Completed,
                SagaRollbackOutcome::Fault,
                SagaRollbackOutcome::Emitted,
                SagaRollbackOutcome::Branched,
                SagaRollbackOutcome::Jumped,
                SagaRollbackOutcome::MissingHandler,
            ] {
                let response = map_local_report(ExecutionReport::Unpersisted {
                    result: next(),
                    cleanup: Some(SagaRollbackReport {
                        trace_id: "private-trace".into(),
                        finished,
                        steps: vec![SagaRollbackStep {
                            node_id: "private-node".into(),
                            outcome,
                        }],
                    }),
                });
                if finished && outcome == SagaRollbackOutcome::Completed {
                    assert_eq!(response.status(), StatusCode::OK);
                } else {
                    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
                    assert_eq!(response.body(), "Reconciliation required");
                }
            }
        }
    }

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
            Ok(map_local_report(report))
        })
    }
}

fn map_local_report(
    report: ExecutionReport<Outcome<DomainResponse, AppError>>,
) -> Response<String> {
    if report
        .cleanup()
        .is_some_and(|cleanup| !cleanup.is_complete())
    {
        return response(StatusCode::SERVICE_UNAVAILABLE, "Reconciliation required");
    }
    let ExecutionReport::Unpersisted { result, .. } = report else {
        return response(StatusCode::SERVICE_UNAVAILABLE, "Reconciliation required");
    };
    match result {
        Outcome::Next(domain) => match StatusCode::from_u16(domain.status) {
            Ok(status) => response(status, &domain.body),
            Err(_) => response(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error"),
        },
        Outcome::Fault(AppError::Unauthenticated) => {
            response(StatusCode::UNAUTHORIZED, "Unauthorized")
        }
        _ => response(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error"),
    }
}
