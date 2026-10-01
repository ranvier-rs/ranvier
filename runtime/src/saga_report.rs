//! Per-invocation Saga rollback observations, available on the caller's Bus.
//! Hook completion does not establish external effect reversal or durability.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SagaRollbackOutcome {
    InProgress,
    Panicked,
    Completed,
    Fault,
    Emitted,
    Branched,
    Jumped,
    MissingHandler,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SagaRollbackStep {
    pub node_id: String,
    pub outcome: SagaRollbackOutcome,
}

/// Ordered hook observations. No raw handler error or snapshot is included.
/// This is a local cleanup report, not a stored checkpoint or retry permission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SagaRollbackReport {
    pub trace_id: String,
    /// False until every queued rollback task has been inspected.
    pub finished: bool,
    pub steps: Vec<SagaRollbackStep>,
}

impl SagaRollbackReport {
    /// True if every requested hook returned Next(()); an empty rollback has
    /// no incomplete hooks. This does not certify any external effect reversal.
    pub fn is_complete(&self) -> bool {
        self.finished
            && self
                .steps
                .iter()
                .all(|step| step.outcome == SagaRollbackOutcome::Completed)
    }
}
