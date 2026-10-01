/*!
# OTLP Concept Example

This example demonstrates how to integrate OpenTelemetry-style tracing into Ranvier Axons.
It uses the `Traced` wrapper to automatically generate spans for Transitions.

## Key Concepts

1.  **ConnectionBus**: Wraps `Bus` to ensure connection ID is available.
2.  **TraceContext**: Carries Trace ID and Span ID (simulated).
3.  **Traced Wrapper**: `Traced::new(inner, "name")` decorates a Transition with logging/tracing logic.

*/

use async_trait::async_trait;
use ranvier_core::bus::{ConnectionBus, ConnectionId};
use ranvier_core::prelude::*;
use ranvier_core::telemetry::Traced;
use ranvier_runtime::Axon;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;

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

// ============================================================================
// 1. Data Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HttpRequest {
    path: String,
    _method: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AuthUser {
    id: String,
    role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HttpResponse {
    status: u16,
    body: String,
}

// ============================================================================
// 2. Transitions
// ============================================================================

/// Transition: Parse Request -> AuthUser (Simulated Auth)
#[derive(Clone, Debug)]
struct Authenticate;

#[async_trait]
impl Transition<HttpRequest, AuthUser> for Authenticate {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: HttpRequest,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<AuthUser, Self::Error> {
        // Access Connection ID if available
        // Note: bus here is &mut Bus, but we know we started with ConnectionBus
        // If we need strict typing, we might use a trait or specialized read.

        // Let's simulate checking a header
        if input.path == "/login" {
            // Fail flow
            return Outcome::Fault("Login not supported in this demo".to_string());
        }

        Outcome::Next(AuthUser {
            id: "user_123".to_string(),
            role: "admin".to_string(),
        })
    }
}

/// Transition: Handle Logic -> HttpResponse
#[derive(Clone, Debug)]
struct HandleRequest;

#[async_trait]
impl Transition<AuthUser, HttpResponse> for HandleRequest {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: AuthUser,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<HttpResponse, Self::Error> {
        Outcome::Next(HttpResponse {
            status: 200,
            body: format!("Hello, {}! You are {}.", input.id, input.role),
        })
    }
}

// ============================================================================
// 3. Main Logic
// ============================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== OTLP Tracing Concept ===\n");

    // 1. Setup Request and Connection Context
    let conn_id = ConnectionId::new();
    let bus = Bus::new();
    let mut conn_bus = ConnectionBus::from_bus(conn_id, bus);

    println!("[System] Connection ID: {:?}", conn_bus.connection_id());

    // 2. Define Axon with Tracing Wrappers
    // Provide a name for each span
    let axon = Axon::<HttpRequest, HttpRequest, String>::new("HttpTransaction")
        .then(Traced::new(Authenticate, "Authenticate"))
        .then(Traced::new(HandleRequest, "HandleRequest"));

    // 3. Execute
    let req_input = HttpRequest {
        path: "/dashboard".to_string(),
        _method: "GET".to_string(),
    };

    // Note: ConnectionBus derefs to Bus, so we can pass it as &mut Bus
    match local_demo_outcome(axon.execute(req_input, &(), &mut conn_bus).await)? {
        Outcome::Next(res) => {
            println!("\n[Result] Status: {}, Body: {}", res.status, res.body);
        }
        Outcome::Fault(e) => {
            println!("\n[Result] Fault: {:?}", e);
        }
        _ => anyhow::bail!("Unexpected tracing control outcome"),
    }

    Ok(())
}

#[cfg(test)]
mod report_tests {
    use super::*;
    use ranvier_runtime::{
        CompletionConfirmation, EffectBoundary, ExecutionReport, PersistenceOperation,
        SagaRollbackOutcome, SagaRollbackReport, SagaRollbackStep,
    };
    #[test]
    fn declared_demo_executes_with_report_policy() {
        main().unwrap();
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
