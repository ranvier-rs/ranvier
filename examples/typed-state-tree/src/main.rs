/*!
# Typed State Tree Example

This example demonstrates how to use Rust enums to represent a "Typed State Tree".
Instead of a simple linear flow, the Axon moves between explicit domain states.

## Key Concepts
1. **Explicit States**: Each stage in the process is represented by a variant in an enum.
2. **Type-Safe Transitions**: Transitions move from one state variant to another.
3. **Outcome Control**: The flow is controlled by returning `Outcome::Next`, `Outcome::Branch`, etc.
*/

use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_runtime::Axon;
use serde::{Deserialize, Serialize};

// ============================================================================
// 1. Define the State Tree
// ============================================================================

/// Represents the possible states of our application flow.
#[derive(Debug, Clone, Serialize, Deserialize)]
enum FlowState {
    /// Initial state: we have a raw request (e.g. URI)
    RequestReceived(String),
    /// State 2: Request has been authenticated, we have a User ID
    Authenticated { user_id: String, path: String },
    /// State 3: Content has been fetched from DB
    ContentLoaded { _user_id: String, data: String },
}

// ============================================================================
// 2. Define Transitions
// ============================================================================

/// Transition: RequestReceived -> Authenticated
#[derive(Debug, Clone)]
struct Authenticate;

#[async_trait]
impl Transition<FlowState, FlowState> for Authenticate {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        state: FlowState,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<FlowState, Self::Error> {
        if let FlowState::RequestReceived(uri) = state {
            println!("[Sync] Authenticating request for: {}", uri);

            // Simulation logic
            if uri == "/forbidden" {
                return Outcome::Fault("Access Denied".to_string());
            }

            Outcome::Next(FlowState::Authenticated {
                user_id: "user_123".to_string(),
                path: uri,
            })
        } else {
            Outcome::Fault("Unexpected State: Expected RequestReceived".to_string())
        }
    }
}

/// Transition: Authenticated -> ContentLoaded
#[derive(Debug, Clone)]
struct FetchContent;

#[async_trait]
impl Transition<FlowState, FlowState> for FetchContent {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        state: FlowState,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<FlowState, Self::Error> {
        if let FlowState::Authenticated { user_id, path } = state {
            println!("[Sync] Fetching content for user {} at {}", user_id, path);

            Outcome::Next(FlowState::ContentLoaded {
                _user_id: user_id,
                data: format!("Secret data for {}", path),
            })
        } else {
            Outcome::Fault("Unexpected State: Expected Authenticated".to_string())
        }
    }
}

// ============================================================================
// 3. Main - Wire the Tree
// ============================================================================

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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Typed State Tree Demo ===\n");

    // Construct the Axon
    // Even though the input/output of the Axon is FlowState, the INTERNAL
    // transitions move the variant forward.
    let axon = Axon::<FlowState, FlowState, String>::new("SecureContentFlow")
        .then(Authenticate)
        .then(FetchContent);

    // Case 1: Valid Path
    println!("--- Case 1: Valid Path ---");
    let input1 = FlowState::RequestReceived("/dashboard".to_string());
    let mut bus = Bus::new();
    match local_demo_outcome(axon.execute(input1, &(), &mut bus).await)? {
        Outcome::Next(FlowState::ContentLoaded { data, .. }) => {
            println!("Success! Final Data: {}", data);
        }
        Outcome::Fault(e) => println!("Error: {}", e),
        _ => anyhow::bail!("Unexpected domain control flow in local demo"),
    }

    // Case 2: Forbidden Path
    println!("\n--- Case 2: Forbidden Path ---");
    let input2 = FlowState::RequestReceived("/forbidden".to_string());
    let mut bus = Bus::new();
    match local_demo_outcome(axon.execute(input2, &(), &mut bus).await)? {
        Outcome::Fault(e) => println!("Caught expected error: {}", e),
        _ => anyhow::bail!("Unexpected domain control flow in local demo"),
    }

    Ok(())
}

#[cfg(test)]
mod execution_contract_tests {
    use super::*;
    use ranvier_runtime::{
        CompletionConfirmation, EffectBoundary, ExecutionReport, PersistenceOperation,
        SagaRollbackOutcome, SagaRollbackReport, SagaRollbackStep,
    };

    #[test]
    fn declared_demo_scenarios_run_through_the_report_policy() {
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
