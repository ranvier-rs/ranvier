//! Custom Error Types Example
//!
//! ## Purpose
//! Demonstrates how to use domain-specific error types with `thiserror` instead of `String`,
//! enabling type-safe error handling and structured error reporting.
//!
//! ## Run
//! ```bash
//! cargo run -p custom-error-types
//! ```
//!
//! ## Key Concepts
//! - Using `thiserror::Error` for ergonomic error enums
//! - Typed errors as `Transition::Error` associated type
//! - Matching on specific error variants
//! - `Outcome::Fault` with custom error types
//!
//! ## Prerequisites
//! - `hello-world` — basic Transition + Axon usage
//! - `testing-patterns` — unit/integration testing strategies
//!
//! ## Next Steps
//! - `retry-dlq-demo` — retry, timeout, and DLQ patterns
//! - `order-processing-demo` — production-style workflow with branching

use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_runtime::Axon;
use serde::{Deserialize, Serialize};

// ============================================================================
// Domain Error Type
// ============================================================================

#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
enum AppError {
    #[error("validation error: {0}")]
    Validation(String),
    #[error("resource not found: {0}")]
    NotFound(String),
    #[error("unauthorized access")]
    Unauthorized,
}

// ============================================================================
// Domain Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UserId(String);

#[derive(Debug, Clone, Serialize, Deserialize)]
struct User {
    id: UserId,
    name: String,
    role: String,
}

// ============================================================================
// Transitions with Typed Errors
// ============================================================================

#[derive(Clone)]
struct ValidateUserId;

#[async_trait]
impl Transition<UserId, UserId> for ValidateUserId {
    type Error = AppError;
    type Resources = ();

    async fn run(
        &self,
        id: UserId,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<UserId, Self::Error> {
        if id.0.is_empty() {
            return Outcome::Fault(AppError::Validation("User ID cannot be empty".into()));
        }
        if id.0.len() < 3 {
            return Outcome::Fault(AppError::Validation(
                "User ID must be at least 3 characters".into(),
            ));
        }
        Outcome::Next(id)
    }
}

#[derive(Clone)]
struct FetchUser;

#[async_trait]
impl Transition<UserId, User> for FetchUser {
    type Error = AppError;
    type Resources = ();

    async fn run(
        &self,
        id: UserId,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<User, Self::Error> {
        match id.0.as_str() {
            "admin" => Outcome::Next(User {
                id: id.clone(),
                name: "Admin User".into(),
                role: "admin".into(),
            }),
            "user123" => Outcome::Next(User {
                id: id.clone(),
                name: "Regular User".into(),
                role: "user".into(),
            }),
            _ => Outcome::Fault(AppError::NotFound(format!("User '{}' not found", id.0))),
        }
    }
}

#[derive(Clone)]
struct CheckAuthorization;

#[async_trait]
impl Transition<User, User> for CheckAuthorization {
    type Error = AppError;
    type Resources = ();

    async fn run(
        &self,
        user: User,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<User, Self::Error> {
        if user.role != "admin" {
            return Outcome::Fault(AppError::Unauthorized);
        }
        Outcome::Next(user)
    }
}

// ============================================================================
// Main
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
    println!("=== Custom Error Types Example ===\n");

    // Test 1: Successful flow
    println!("Test 1: Valid admin user");
    let axon = Axon::<UserId, UserId, AppError, ()>::new("auth.pipeline")
        .then(ValidateUserId)
        .then(FetchUser)
        .then(CheckAuthorization);

    let mut bus = Bus::new();
    match local_demo_outcome(axon.execute(UserId("admin".into()), &(), &mut bus).await)? {
        Outcome::Next(user) => {
            println!("  Success: {} (role={})", user.name, user.role);
        }
        Outcome::Fault(err) => {
            println!("  Error: {}", err);
        }
        _ => anyhow::bail!("Unexpected domain control flow in local demo"),
    }

    // Test 2: Validation error
    println!("\nTest 2: Invalid user ID (too short)");
    let axon = Axon::<UserId, UserId, AppError, ()>::new("auth.validate")
        .then(ValidateUserId)
        .then(FetchUser);

    let mut bus = Bus::new();
    match local_demo_outcome(axon.execute(UserId("ab".into()), &(), &mut bus).await)? {
        Outcome::Next(user) => {
            println!("  Success: {:?}", user);
        }
        Outcome::Fault(err) => {
            println!("  Error: {}", err);
            match &err {
                AppError::Validation(msg) => println!("  -> Validation detail: {}", msg),
                _ => println!("  -> Unexpected error variant"),
            }
        }
        _ => anyhow::bail!("Unexpected domain control flow in local demo"),
    }

    // Test 3: Not found error
    println!("\nTest 3: Non-existent user");
    let axon = Axon::<UserId, UserId, AppError, ()>::new("auth.lookup")
        .then(ValidateUserId)
        .then(FetchUser);

    let mut bus = Bus::new();
    match local_demo_outcome(axon.execute(UserId("unknown".into()), &(), &mut bus).await)? {
        Outcome::Next(user) => {
            println!("  Success: {:?}", user);
        }
        Outcome::Fault(err) => {
            println!("  Error: {}", err);
            match &err {
                AppError::NotFound(resource) => println!("  -> Not found: {}", resource),
                _ => println!("  -> Unexpected error variant"),
            }
        }
        _ => anyhow::bail!("Unexpected domain control flow in local demo"),
    }

    // Test 4: Authorization error
    println!("\nTest 4: Unauthorized user");
    let axon = Axon::<UserId, UserId, AppError, ()>::new("auth.full")
        .then(ValidateUserId)
        .then(FetchUser)
        .then(CheckAuthorization);

    let mut bus = Bus::new();
    match local_demo_outcome(axon.execute(UserId("user123".into()), &(), &mut bus).await)? {
        Outcome::Next(user) => {
            println!("  Success: {:?}", user);
        }
        Outcome::Fault(err) => {
            println!("  Error: {}", err);
            match &err {
                AppError::Unauthorized => println!("  -> Access denied"),
                _ => println!("  -> Unexpected error variant"),
            }
        }
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
