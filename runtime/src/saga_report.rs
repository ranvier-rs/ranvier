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

/// Invocation-owned cleanup journal. Cloning this handle does not copy steps.
/// Call snapshot to obtain an owned serializable report, including partial work.
#[derive(Debug, Clone)]
pub struct SagaRollbackJournal(std::sync::Arc<std::sync::Mutex<SagaRollbackReport>>);

impl SagaRollbackJournal {
    pub(crate) fn new(trace_id: &str) -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(
            SagaRollbackReport {
                trace_id: trace_id.to_string(),
                finished: false,
                steps: Vec::new(),
            },
        )))
    }
    pub fn snapshot(&self) -> SagaRollbackReport {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
    pub(crate) fn push(&self, node_id: String, outcome: SagaRollbackOutcome) {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .steps
            .push(SagaRollbackStep { node_id, outcome });
    }
    pub(crate) fn settle(&self, outcome: SagaRollbackOutcome) {
        let mut report = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(step) = report.steps.last_mut() {
            step.outcome = outcome;
        }
    }
    pub(crate) fn finish(&self) {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .finished = true;
    }
}
