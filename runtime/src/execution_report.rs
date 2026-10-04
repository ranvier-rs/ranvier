//! Execution result contract for the versioned durability cutover.
//!
//! These types do not turn an existing Outcome into durability evidence. The
//! executor must construct a report from actual store acknowledgements. Wiring
//! ordinary and cancellation execution, and migrating consumers, are separate
//! required parts of that cutover.

use crate::saga_report::SagaRollbackReport;
use serde::{Deserialize, Serialize};

/// Observation of a single store call. An error cannot establish non-commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreConfirmation {
    Confirmed,
    Unconfirmed,
}

/// Persistence operation classification; backend text and credentials are excluded.
/// Result/Terminal can also identify encoding failure before an append is called.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersistenceOperation {
    Load,
    /// Loaded history contains an unusable recovery payload; operator reconciliation is required.
    RecoveryPayload,
    /// The stored format or event sequence is not valid for this release.
    UnsupportedFormat,
    /// This effect has no event-journal contract in the configured persistence mode.
    UnsupportedEffect,
    /// A confirmed intent has no confirmed matching result; never replay it automatically.
    PendingIntent,
    Intent,
    Result,
    Terminal,
    /// The execution trace's compensation event, separate from marker-store writes.
    CompensationEvent,
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
    /// No execution-trace store was configured. A separate cleanup marker may
    /// still have been recorded; this never implies that the execution trace is durable.
    Unpersisted {
        result: T,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cleanup: Option<SagaRollbackReport>,
    },
    /// A store was configured, but execution returned without requesting writes.
    NotRequested {
        result: T,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cleanup: Option<SagaRollbackReport>,
    },
    /// Selected event writes were acknowledged. Completion may be app-owned.
    Recorded {
        result: T,
        completion: CompletionConfirmation,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cleanup: Option<SagaRollbackReport>,
    },
    /// Preserve the known result, without certifying the failed persistence operation.
    /// Encoding failure can prevent the selected append from being attempted;
    /// earlier effects or writes can still require reconciliation.
    Unconfirmed {
        result: T,
        operation: PersistenceOperation,
        effect: EffectBoundary,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cleanup: Option<SagaRollbackReport>,
    },
    /// Required persistence prevented the next effect; no terminal domain result
    /// exists. Earlier effects in the workflow may already have been invoked.
    BlockedBeforeEffect {
        operation: PersistenceOperation,
        effect: EffectBoundary,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cleanup: Option<SagaRollbackReport>,
    },
}

impl<T> ExecutionReport<T> {
    /// Transform the owned result while retaining every durability and cleanup observation.
    ///
    /// This is a value transformation, not permission to invoke another effect. The
    /// callback runs for `Unconfirmed` too, so callers must inspect the report before
    /// starting stream initialization or any other business operation. It never runs
    /// for `BlockedBeforeEffect`, which has no result to transform.
    pub fn map_result<U>(self, map: impl FnOnce(T) -> U) -> ExecutionReport<U> {
        match self {
            Self::Unpersisted { result, cleanup } => ExecutionReport::Unpersisted {
                result: map(result),
                cleanup,
            },
            Self::NotRequested { result, cleanup } => ExecutionReport::NotRequested {
                result: map(result),
                cleanup,
            },
            Self::Recorded {
                result,
                completion,
                cleanup,
            } => ExecutionReport::Recorded {
                result: map(result),
                completion,
                cleanup,
            },
            Self::Unconfirmed {
                result,
                operation,
                effect,
                cleanup,
            } => ExecutionReport::Unconfirmed {
                result: map(result),
                operation,
                effect,
                cleanup,
            },
            Self::BlockedBeforeEffect {
                operation,
                effect,
                cleanup,
            } => ExecutionReport::BlockedBeforeEffect {
                operation,
                effect,
                cleanup,
            },
        }
    }
    /// Owned cleanup observations are independent of domain and durability state.
    /// Absence means no Saga rollback report was observed for this invocation.
    pub fn cleanup(&self) -> Option<&SagaRollbackReport> {
        match self {
            Self::Unpersisted { cleanup, .. }
            | Self::NotRequested { cleanup, .. }
            | Self::Recorded { cleanup, .. }
            | Self::Unconfirmed { cleanup, .. }
            | Self::BlockedBeforeEffect { cleanup, .. } => cleanup.as_ref(),
        }
    }
    pub(crate) fn with_cleanup(mut self, report: Option<SagaRollbackReport>) -> Self {
        let target = match &mut self {
            Self::Unpersisted { cleanup, .. }
            | Self::NotRequested { cleanup, .. }
            | Self::Recorded { cleanup, .. }
            | Self::Unconfirmed { cleanup, .. }
            | Self::BlockedBeforeEffect { cleanup, .. } => cleanup,
        };
        *target = report;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_mapping_preserves_all_observations_and_callback_count() {
        use crate::saga_report::{SagaRollbackOutcome, SagaRollbackStep};
        let cleanup = SagaRollbackReport {
            trace_id: "partial-cleanup".into(),
            finished: false,
            steps: vec![SagaRollbackStep {
                node_id: "undo".into(),
                outcome: SagaRollbackOutcome::InProgress,
            }],
        };
        let reports = vec![
            ExecutionReport::Unpersisted {
                result: String::from("known"),
                cleanup: None,
            },
            ExecutionReport::NotRequested {
                result: String::from("known"),
                cleanup: None,
            },
            ExecutionReport::Recorded {
                result: String::from("known"),
                completion: CompletionConfirmation::Confirmed,
                cleanup: None,
            },
            ExecutionReport::Recorded {
                result: String::from("known"),
                completion: CompletionConfirmation::NotRequested,
                cleanup: None,
            },
        ];
        let mut reports = reports;
        for effect in [EffectBoundary::NotInvoked, EffectBoundary::MayHaveRun] {
            for operation in [
                PersistenceOperation::Load,
                PersistenceOperation::RecoveryPayload,
                PersistenceOperation::Intent,
                PersistenceOperation::Result,
                PersistenceOperation::Terminal,
                PersistenceOperation::Complete,
                PersistenceOperation::CompensationRead,
                PersistenceOperation::CompensationWrite,
                PersistenceOperation::CancellationCursor,
            ] {
                reports.push(ExecutionReport::Unconfirmed {
                    result: String::from("known"),
                    operation,
                    effect,
                    cleanup: None,
                });
                reports.push(ExecutionReport::BlockedBeforeEffect {
                    operation,
                    effect,
                    cleanup: None,
                });
            }
        }
        for report in reports {
            for attached in [None, Some(cleanup.clone())] {
                let report = report.clone().with_cleanup(attached);
                let mut expected = serde_json::to_value(&report).unwrap();
                let has_result = expected.get("result").is_some();
                if has_result {
                    expected["result"] = serde_json::json!(["known"]);
                }
                let mut calls = 0;
                let mapped = report.map_result(|result| {
                    calls += 1;
                    vec![result]
                });
                assert_eq!(calls, usize::from(has_result));
                assert_eq!(serde_json::to_value(&mapped).unwrap(), expected);
                assert_eq!(
                    mapped.cleanup().map(|r| r.is_complete()),
                    if mapped.cleanup().is_some() {
                        Some(false)
                    } else {
                        None
                    }
                );
            }
        }
    }

    #[test]
    fn result_mapping_moves_non_clone_values_and_fn_once_captures() {
        struct Owned(String);
        let report = ExecutionReport::Unconfirmed {
            result: Owned("known-result".into()),
            operation: PersistenceOperation::Complete,
            effect: EffectBoundary::MayHaveRun,
            cleanup: None,
        };
        let capture = Owned("owned-capture".into());
        let mapped = report.map_result(move |value| (value, capture));
        match mapped {
            ExecutionReport::Unconfirmed {
                result: (value, capture),
                operation,
                effect,
                cleanup,
            } => {
                assert_eq!(value.0, "known-result");
                assert_eq!(capture.0, "owned-capture");
                assert_eq!(operation, PersistenceOperation::Complete);
                assert_eq!(effect, EffectBoundary::MayHaveRun);
                assert!(cleanup.is_none());
            }
            _ => panic!("mapping changed durability classification"),
        }
        let blocked = ExecutionReport::<Owned>::BlockedBeforeEffect {
            operation: PersistenceOperation::Intent,
            effect: EffectBoundary::MayHaveRun,
            cleanup: None,
        };
        let mapped: ExecutionReport<Owned> =
            blocked.map_result(|_| panic!("blocked result was fabricated"));
        assert!(matches!(
            mapped,
            ExecutionReport::BlockedBeforeEffect {
                operation: PersistenceOperation::Intent,
                effect: EffectBoundary::MayHaveRun,
                cleanup: None,
            }
        ));
    }

    #[test]
    fn completion_omission_is_distinct_from_confirmation() {
        let confirmed = ExecutionReport::Recorded {
            result: 7,
            completion: CompletionConfirmation::Confirmed,
            cleanup: None,
        };
        let omitted = ExecutionReport::Recorded {
            result: 7,
            completion: CompletionConfirmation::NotRequested,
            cleanup: None,
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
            cleanup: None,
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
            cleanup: None,
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
            cleanup: None,
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
            cleanup: None,
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
    #[test]
    fn cleanup_roundtrip_preserves_every_durability_variant() {
        use crate::saga_report::{SagaRollbackOutcome, SagaRollbackReport, SagaRollbackStep};
        let cleanup = SagaRollbackReport {
            trace_id: "own-trace".into(),
            finished: true,
            steps: vec![SagaRollbackStep {
                node_id: "undo".into(),
                outcome: SagaRollbackOutcome::Fault,
            }],
        };
        for report in [
            ExecutionReport::Unpersisted {
                result: 7,
                cleanup: None,
            },
            ExecutionReport::NotRequested {
                result: 7,
                cleanup: None,
            },
            ExecutionReport::Recorded {
                result: 7,
                completion: CompletionConfirmation::Confirmed,
                cleanup: None,
            },
            ExecutionReport::Unconfirmed {
                result: 7,
                operation: PersistenceOperation::Result,
                effect: EffectBoundary::MayHaveRun,
                cleanup: None,
            },
            ExecutionReport::BlockedBeforeEffect {
                operation: PersistenceOperation::Intent,
                effect: EffectBoundary::MayHaveRun,
                cleanup: None,
            },
        ] {
            let report = report.with_cleanup(Some(cleanup.clone()));
            assert_eq!(report.cleanup(), Some(&cleanup));
            let json = serde_json::to_value(&report).unwrap();
            assert_eq!(json["cleanup"]["steps"][0]["outcome"], "fault");
            let decoded: ExecutionReport<u32> = serde_json::from_value(json).unwrap();
            assert_eq!(decoded, report);
        }
    }
}
