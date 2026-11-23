// Extracted public policy: only HTTP rejection expressions become false.
// This does not compile Actix/Axum imports, extractors or response bodies.
#[rustfmt::skip] // Preserve the exact exported source for drift checking.
mod public_policy {
use ranvier::{ExecutionReport, CompletionConfirmation, EffectBoundary, PersistenceOperation, SagaRollbackReport, SagaRollbackStep, SagaRollbackOutcome};
use ranvier::prelude::Outcome;
fn policy_0(report: ExecutionReport<Outcome<u32, String>>) -> bool {
    if report.cleanup().is_some_and(|cleanup| !cleanup.is_complete()) {
        return false;
    }
    // Unpersisted is intentional for this local example; require confirmed completion when recorded.
    let outcome = match report {
        ExecutionReport::Unpersisted { result, .. }
        | ExecutionReport::Recorded { result, completion: CompletionConfirmation::Confirmed, .. } => result,
        _ => return false,
    };

    matches!(outcome, Outcome::Next(_) | Outcome::Fault(_))
}

fn policy_1(report: ExecutionReport<Outcome<u32, String>>) -> bool {
    if report.cleanup().is_some_and(|cleanup| !cleanup.is_complete()) {
        return false;
    }
    let outcome = match report {
        ExecutionReport::Unpersisted { result, .. }
        | ExecutionReport::Recorded { result, completion: CompletionConfirmation::Confirmed, .. } => result,
        _ => return false,
    };

    matches!(outcome, Outcome::Next(_) | Outcome::Fault(_))
}

fn policy_2(report: ExecutionReport<Outcome<u32, String>>) -> bool {
    if report.cleanup().is_some_and(|cleanup| !cleanup.is_complete()) {
        return false;
    }
    // Unpersisted is intentional for this local example; require confirmed completion when recorded.
    let outcome = match report {
        ExecutionReport::Unpersisted { result, .. }
        | ExecutionReport::Recorded { result, completion: CompletionConfirmation::Confirmed, .. } => result,
        _ => return false,
    };

    matches!(outcome, Outcome::Next(_) | Outcome::Fault(_))
}

fn policy_3(report: ExecutionReport<Outcome<u32, String>>) -> bool {
    if report.cleanup().is_some_and(|cleanup| !cleanup.is_complete()) {
        return false;
    }
    let outcome = match report {
        ExecutionReport::Unpersisted { result, .. }
        | ExecutionReport::Recorded { result, completion: CompletionConfirmation::Confirmed, .. } => result,
        _ => return false,
    };

    matches!(outcome, Outcome::Next(_) | Outcome::Fault(_))
}

#[test]
fn public_http_policy_checks_durability_and_cleanup() {
    for policy in [policy_0, policy_1, policy_2, policy_3] {
        for result in [Outcome::Next(7), Outcome::Fault("domain".into())] {
            assert!(policy(ExecutionReport::Unpersisted { result: result.clone(), cleanup: None }));
            assert!(policy(ExecutionReport::Recorded { result: result.clone(), completion: CompletionConfirmation::Confirmed, cleanup: None }));
            assert!(!policy(ExecutionReport::NotRequested { result: result.clone(), cleanup: None }));
            assert!(!policy(ExecutionReport::Recorded { result: result.clone(), completion: CompletionConfirmation::NotRequested, cleanup: None }));
            assert!(!policy(ExecutionReport::Unconfirmed { result: result.clone(), operation: PersistenceOperation::Result, effect: EffectBoundary::MayHaveRun, cleanup: None }));
            assert!(!policy(ExecutionReport::BlockedBeforeEffect { operation: PersistenceOperation::RecoveryPayload, effect: EffectBoundary::MayHaveRun, cleanup: None }));
            assert!(!policy(ExecutionReport::Unpersisted { result, cleanup: Some(SagaRollbackReport { trace_id: "private".into(), finished: false, steps: vec![] }) }));
        }
        for finished in [false, true] {
            for outcome in [SagaRollbackOutcome::InProgress, SagaRollbackOutcome::Panicked,
                SagaRollbackOutcome::Completed, SagaRollbackOutcome::Fault, SagaRollbackOutcome::Emitted,
                SagaRollbackOutcome::Branched, SagaRollbackOutcome::Jumped, SagaRollbackOutcome::MissingHandler] {
                let cleanup = SagaRollbackReport { trace_id: "private".into(), finished,
                    steps: vec![SagaRollbackStep { node_id: "private".into(), outcome }] };
                let allowed = finished && outcome == SagaRollbackOutcome::Completed;
                assert_eq!(policy(ExecutionReport::Recorded { result: Outcome::Next(7),
                    completion: CompletionConfirmation::Confirmed, cleanup: Some(cleanup) }), allowed);
            }
        }
    }
}
}
