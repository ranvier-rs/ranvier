//! Execution result contract for the versioned durability cutover.
//!
//! These types do not turn an existing Outcome into durability evidence. The
//! executor must construct a report from actual store acknowledgements. Wiring
//! ordinary and cancellation execution, and migrating consumers, are separate
//! required parts of that cutover.

use serde::{Deserialize, Serialize};

/// Store operation classification; backend text and credentials are excluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersistenceOperation {
    Load,
    Intent,
    Result,
    Terminal,
    Complete,
    CompensationRead,
    CompensationWrite,
    CancellationCursor,
}

/// Whether execution may already have invoked a business or cleanup effect.
/// This is not a statement about whether that effect committed externally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectBoundary {
    NotInvoked,
    MayHaveRun,
}

/// Completion is separate from acknowledgement of preceding event writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionConfirmation {
    Confirmed,
    NotRequested,
}

/// Owned domain/infrastructure result and the observed durability boundary.
///
/// `T` is the ordinary domain result or the cancellation-aware terminal result.
/// No variant authorizes automatic retry. In particular an unconfirmed write
/// may have committed; reconciliation needs authoritative readback or an
/// effect adapter's explicit idempotency contract.
///
/// There is deliberately no Outcome conversion or default: neither can infer
/// store acknowledgements. This is not the persisted event wire format.
#[must_use = "inspect the execution result and its durability before acknowledging success"]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "durability", rename_all = "snake_case")]
pub enum ExecutionReport<T> {
    /// No persistence store was configured. No durable success is implied.
    Unpersisted { result: T },
    /// Selected event writes were acknowledged. Completion may be app-owned.
    Recorded {
        result: T,
        completion: CompletionConfirmation,
    },
    /// Preserve the known result, without certifying the failed store operation.
    Unconfirmed {
        result: T,
        operation: PersistenceOperation,
        effect: EffectBoundary,
    },
    /// Required persistence prevented the next effect; no terminal domain result
    /// exists. Earlier effects in the workflow may already have been invoked.
    BlockedBeforeEffect {
        operation: PersistenceOperation,
        effect: EffectBoundary,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_omission_is_distinct_from_confirmation() {
        let confirmed = ExecutionReport::Recorded {
            result: 7,
            completion: CompletionConfirmation::Confirmed,
        };
        let omitted = ExecutionReport::Recorded {
            result: 7,
            completion: CompletionConfirmation::NotRequested,
        };
        assert_ne!(confirmed, omitted);
        assert_ne!(
            serde_json::to_value(confirmed).unwrap(),
            serde_json::to_value(omitted).unwrap()
        );
    }

    #[test]
    fn blocked_execution_has_no_fabricated_result() {
        let report = ExecutionReport::<u32>::BlockedBeforeEffect {
            operation: PersistenceOperation::Intent,
            effect: EffectBoundary::NotInvoked,
        };
        let json = serde_json::to_value(&report).unwrap();
        assert!(json.get("result").is_none());
        assert_eq!(json["durability"], "blocked_before_effect");
        assert_eq!(
            serde_json::from_value::<ExecutionReport<u32>>(json).unwrap(),
            report
        );
    }

    #[test]
    fn later_block_does_not_erase_prior_effect_boundary() {
        let report = ExecutionReport::<u32>::BlockedBeforeEffect {
            operation: PersistenceOperation::Intent,
            effect: EffectBoundary::MayHaveRun,
        };
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["effect"], "may_have_run");
        assert!(json.get("result").is_none());
        assert_eq!(
            serde_json::from_value::<ExecutionReport<u32>>(json).unwrap(),
            report
        );
    }

    #[test]
    fn business_failure_survives_unconfirmed_completion() {
        let report = ExecutionReport::Unconfirmed {
            result: Result::<u32, &str>::Err("business_rejected"),
            operation: PersistenceOperation::Complete,
            effect: EffectBoundary::MayHaveRun,
        };
        let json = serde_json::to_value(report).unwrap();
        assert_eq!(json["result"]["Err"], "business_rejected");
        assert_eq!(json["durability"], "unconfirmed");
    }

    #[test]
    fn unconfirmed_preserves_result_and_safe_classification() {
        let report = ExecutionReport::Unconfirmed {
            result: serde_json::json!({"business": "completed"}),
            operation: PersistenceOperation::Complete,
            effect: EffectBoundary::MayHaveRun,
        };
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["result"]["business"], "completed");
        assert_eq!(json["operation"], "complete");
        assert_eq!(json["effect"], "may_have_run");
        assert_eq!(json.as_object().unwrap().len(), 4);
        assert_eq!(
            serde_json::from_value::<ExecutionReport<serde_json::Value>>(json).unwrap(),
            report
        );
    }
}
