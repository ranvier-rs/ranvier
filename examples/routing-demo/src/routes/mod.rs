// Explicit local-demo policy: neither persisted nor incomplete cleanup results
// may be converted into ordinary domain success or automatic retry permission.
fn local_demo_outcome<T, E>(
    report: ranvier_runtime::ExecutionReport<Outcome<T, E>>,
) -> anyhow::Result<Outcome<T, E>> {
    if report
        .cleanup()
        .is_some_and(|cleanup| !cleanup.is_complete())
    {
        anyhow::bail!("Execution requires reconciliation before retry");
    }
    match report {
        ranvier_runtime::ExecutionReport::Unpersisted { result, .. } => Ok(result),
        _ => anyhow::bail!("Execution requires reconciliation before retry"),
    }
}

use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_runtime::Axon;
use serde::{Deserialize, Serialize};

pub mod api;

/// Represents an incoming route request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteRequest {
    pub method: HttpMethod,
    pub path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[expect(
    clippy::upper_case_acronyms,
    reason = "variant names are the HTTP method tokens and their serialized form"
)]
pub enum HttpMethod {
    GET,
    POST,
    PUT,
    DELETE,
}

impl HttpMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            HttpMethod::GET => "GET",
            HttpMethod::POST => "POST",
            HttpMethod::PUT => "PUT",
            HttpMethod::DELETE => "DELETE",
        }
    }
}

/// Represents a route response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteResponse {
    pub status: u16,
    pub body: String,
}

/// Error type for routing
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub enum RouteError {
    NotFound(String),
    MethodNotAllowed(String),
    BadRequest(String),
}

// ============================================================================
// Demo Functions
// ============================================================================

pub async fn demo_path_routing() -> anyhow::Result<()> {
    // Test different routes
    let requests = vec![
        RouteRequest {
            method: HttpMethod::GET,
            path: "/".to_string(),
        },
        RouteRequest {
            method: HttpMethod::GET,
            path: "/status".to_string(),
        },
        RouteRequest {
            method: HttpMethod::POST,
            path: "/submit".to_string(),
        },
        RouteRequest {
            method: HttpMethod::GET,
            path: "/unknown".to_string(),
        },
    ];

    for req in requests {
        println!("  {} {} => ", req.method.as_str(), req.path);
        let mut bus = Bus::new();
        let axon =
            Axon::<RouteRequest, RouteRequest, RouteError>::new("RootRouter").then(RootRoute);
        match local_demo_outcome(axon.execute(req, &(), &mut bus).await)? {
            Outcome::Next(resp) => println!("    {} {}", resp.status, resp.body),
            Outcome::Fault(e) => println!("    Error: {:?}", e),
            _ => anyhow::bail!("Unexpected routing control flow"),
        }
    }
    Ok(())
}

pub async fn demo_nested_routing() -> anyhow::Result<()> {
    let requests = vec![
        RouteRequest {
            method: HttpMethod::GET,
            path: "/api/v1/users".to_string(),
        },
        RouteRequest {
            method: HttpMethod::POST,
            path: "/api/v1/users".to_string(),
        },
        RouteRequest {
            method: HttpMethod::GET,
            path: "/api/v1/users/123".to_string(),
        },
        RouteRequest {
            method: HttpMethod::GET,
            path: "/api/v1/users/123/posts".to_string(),
        },
    ];

    for req in requests {
        println!("  {} {} => ", req.method.as_str(), req.path);
        let mut bus = Bus::new();
        let axon = Axon::<RouteRequest, RouteRequest, RouteError>::new("ApiRouter").then(ApiRoute);
        match local_demo_outcome(axon.execute(req.clone(), &(), &mut bus).await)? {
            Outcome::Next(resp) => println!("    {} {}", resp.status, resp.body),
            Outcome::Branch(route, resp_box) => {
                println!("    Branch: {} => {:?}", route, resp_box);
            }
            Outcome::Fault(e) => println!("    Error: {:?}", e),
            _ => anyhow::bail!("Unexpected routing control flow"),
        }
    }
    Ok(())
}

pub async fn demo_branch_routing() -> anyhow::Result<()> {
    let requests = vec![
        RouteRequest {
            method: HttpMethod::GET,
            path: "/admin/users".to_string(),
        },
        RouteRequest {
            method: HttpMethod::GET,
            path: "/api/posts".to_string(),
        },
        RouteRequest {
            method: HttpMethod::GET,
            path: "/public/home".to_string(),
        },
    ];

    for req in requests {
        println!("  {} {} => ", req.method.as_str(), req.path);
        let mut bus = Bus::new();
        let axon =
            Axon::<RouteRequest, RouteRequest, RouteError>::new("BranchRouter").then(BranchRoute);
        let result = local_demo_outcome(axon.execute(req.clone(), &(), &mut bus).await)?;
        match result {
            Outcome::Branch(route, req_val) => {
                println!("    Routed to: {} (payload: {:?})", route, req_val);
            }
            Outcome::Next(req) => println!("    Default route (path: {})", req.path),
            Outcome::Fault(e) => println!("    Error: {:?}", e),
            _ => anyhow::bail!("Unexpected routing control flow"),
        }
    }

    Ok(())
}

// ============================================================================
// Route Transitions
// ============================================================================

/// Root-level routing transition
#[derive(Clone)]
struct RootRoute;

#[async_trait]
impl Transition<RouteRequest, RouteResponse> for RootRoute {
    type Error = RouteError;
    type Resources = ();

    async fn run(
        &self,
        req: RouteRequest,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<RouteResponse, Self::Error> {
        match (req.method, req.path.as_str()) {
            (HttpMethod::GET, "/") => Outcome::Next(RouteResponse {
                status: 200,
                body: "Routing Demo Root".to_string(),
            }),
            (HttpMethod::GET, "/status") => Outcome::Next(RouteResponse {
                status: 200,
                body: "Server is running".to_string(),
            }),
            (HttpMethod::POST, "/submit") => Outcome::Next(RouteResponse {
                status: 201,
                body: "Resource created".to_string(),
            }),
            _ => Outcome::Fault(RouteError::NotFound(req.path)),
        }
    }
}

/// API routing transition with branching
#[derive(Clone)]
struct ApiRoute;

#[async_trait]
impl Transition<RouteRequest, RouteResponse> for ApiRoute {
    type Error = RouteError;
    type Resources = ();

    async fn run(
        &self,
        req: RouteRequest,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<RouteResponse, Self::Error> {
        let full_path = req.path.clone();
        if !path_prefix(&full_path, "/api") {
            return Outcome::Fault(RouteError::NotFound(full_path));
        }

        let api_path = full_path.strip_prefix("/api").unwrap_or("");
        if api_path.is_empty() || api_path == "/" {
            return Outcome::Next(RouteResponse {
                status: 200,
                body: "API Root".to_string(),
            });
        }

        if let Some(v1_path) = api_path
            .strip_prefix("/v1")
            .filter(|rest| rest.is_empty() || rest.starts_with('/'))
        {
            return match api::v1::route_v1(req, v1_path).await {
                Ok(outcome) => outcome,
                Err(error) => Outcome::Fault(RouteError::BadRequest(format!(
                    "Failed to route /api/v1 request: {error}"
                ))),
            };
        }

        Outcome::Fault(RouteError::NotFound(full_path))
    }
}

/// Demonstrates Branch outcome for routing
#[derive(Clone)]
struct BranchRoute;

#[async_trait]
impl Transition<RouteRequest, RouteRequest> for BranchRoute {
    type Error = RouteError;
    type Resources = ();

    async fn run(
        &self,
        req: RouteRequest,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<RouteRequest, Self::Error> {
        let route = if path_prefix(&req.path, "/admin") {
            "admin"
        } else if path_prefix(&req.path, "/api") {
            "api"
        } else if path_prefix(&req.path, "/public") {
            "public"
        } else {
            return Outcome::Next(req);
        };
        match serde_json::to_value(&req) {
            Ok(payload) => Outcome::Branch(route.into(), Some(payload)),
            Err(_) => Outcome::Fault(RouteError::BadRequest(
                "Route payload encoding failed".into(),
            )),
        }
    }
}

fn path_prefix(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

#[cfg(test)]
mod execution_contract_tests {
    use super::*;
    use ranvier_runtime::{
        CompletionConfirmation, EffectBoundary, ExecutionReport, PersistenceOperation,
        SagaRollbackOutcome, SagaRollbackReport, SagaRollbackStep,
    };

    #[tokio::test]
    async fn route_prefixes_match_whole_segments_only() {
        for (path, expected) in [
            ("/admin", Some("admin")),
            ("/admin/users", Some("admin")),
            ("/administrator", None),
            ("/apiary", None),
            ("/publicity", None),
        ] {
            let mut bus = Bus::new();
            let result = BranchRoute
                .run(
                    RouteRequest {
                        method: HttpMethod::GET,
                        path: path.into(),
                    },
                    &(),
                    &mut bus,
                )
                .await;
            match result {
                Outcome::Branch(route, Some(_)) => assert_eq!(Some(route.as_str()), expected),
                Outcome::Next(_) => assert!(expected.is_none()),
                _ => panic!("unexpected routing result"),
            }
        }
        let mut bus = Bus::new();
        assert!(matches!(
            ApiRoute
                .run(
                    RouteRequest {
                        method: HttpMethod::GET,
                        path: "/api/v10/users".into()
                    },
                    &(),
                    &mut bus
                )
                .await,
            Outcome::Fault(RouteError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn declared_routing_scenarios_run_through_the_report_policy() {
        demo_path_routing().await.unwrap();
        demo_nested_routing().await.unwrap();
        demo_branch_routing().await.unwrap();
    }

    #[test]
    fn local_report_policy_preserves_faults_and_rejects_uncertain_or_incomplete_execution() {
        let next = || Outcome::<u32, String>::Next(7);
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
            assert_eq!(
                local_demo_outcome(report).unwrap_err().to_string(),
                "Execution requires reconciliation before retry"
            );
        }
        for (finished, outcome) in [
            (false, SagaRollbackOutcome::Completed),
            (true, SagaRollbackOutcome::Fault),
            (true, SagaRollbackOutcome::InProgress),
        ] {
            let report = ExecutionReport::Unpersisted {
                result: next(),
                cleanup: Some(SagaRollbackReport {
                    trace_id: "private-trace".into(),
                    finished,
                    steps: vec![SagaRollbackStep {
                        node_id: "private-node".into(),
                        outcome,
                    }],
                }),
            };
            assert_eq!(
                local_demo_outcome(report).unwrap_err().to_string(),
                "Execution requires reconciliation before retry"
            );
        }
        assert!(matches!(
            local_demo_outcome(ExecutionReport::Unpersisted {
                result: next(),
                cleanup: None
            })
            .unwrap(),
            Outcome::Next(7)
        ));
        assert!(
            matches!(local_demo_outcome(ExecutionReport::Unpersisted { result: Outcome::<u32, String>::Fault("expected-domain-failure".into()), cleanup: None }).unwrap(), Outcome::Fault(error) if error == "expected-domain-failure")
        );
    }
}
