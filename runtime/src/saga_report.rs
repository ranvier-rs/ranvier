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
#[derive(Clone)]
pub struct SagaRollbackJournal(
    std::sync::Arc<std::sync::Mutex<SagaRollbackReport>>,
    std::sync::Arc<std::sync::Mutex<Vec<(usize, ranvier_core::saga::SagaTask)>>>,
);
impl std::fmt::Debug for SagaRollbackJournal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SagaRollbackJournal")
            .field("report", &self.snapshot())
            .finish()
    }
}

impl SagaRollbackJournal {
    pub(crate) fn new(trace_id: &str) -> Self {
        Self(
            std::sync::Arc::new(std::sync::Mutex::new(SagaRollbackReport {
                trace_id: trace_id.to_string(),
                finished: false,
                steps: Vec::new(),
            })),
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        )
    }
    /// Owned local reconciliation inputs. These may contain secrets: never expose
    /// them as public responses/logs. Retention does not authorize replay or prove
    /// durable recovery; typed resources and graph handler ownership are still needed.
    pub fn unresolved_tasks(&self) -> Vec<ranvier_core::saga::SagaTask> {
        self.1
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .map(|(_, task)| task.clone())
            .collect()
    }
    pub(crate) fn push_task(&self, task: ranvier_core::saga::SagaTask) {
        let mut report = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let index = report.steps.len();
        report.finished = false;
        report.steps.push(SagaRollbackStep {
            node_id: task.node_id.clone(),
            outcome: SagaRollbackOutcome::InProgress,
        });
        self.1
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push((index, task));
    }
    pub fn snapshot(&self) -> SagaRollbackReport {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
    pub(crate) fn matches_trace(&self, trace_id: &str) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .trace_id
            == trace_id
    }
    pub(crate) fn push(&self, node_id: String, outcome: SagaRollbackOutcome) {
        let mut report = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        report.finished = false;
        report.steps.push(SagaRollbackStep { node_id, outcome });
    }
    pub(crate) fn settle(&self, outcome: SagaRollbackOutcome) {
        let mut report = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(step) = report.steps.last_mut() {
            step.outcome = outcome;
        }
        if outcome == SagaRollbackOutcome::Completed {
            let index = report.steps.len().saturating_sub(1);
            self.1
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .retain(|(observed, _)| *observed != index);
        }
    }
    pub(crate) fn finish(&self) {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .finished = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn long_journal_clones_share_storage_and_snapshots_are_owned() {
        let journal = SagaRollbackJournal::new("long-journal");
        let published = journal.clone();
        assert!(std::sync::Arc::ptr_eq(&journal.0, &published.0));
        for index in 0..10_000 {
            journal.push(index.to_string(), SagaRollbackOutcome::InProgress);
            journal.settle(SagaRollbackOutcome::Completed);
        }
        journal.finish();
        let snapshot = published.snapshot();
        assert_eq!(snapshot.steps.len(), 10_000);
        assert!(snapshot.is_complete());
        journal.push("later".into(), SagaRollbackOutcome::InProgress);
        assert_eq!(snapshot.steps.len(), 10_000);
        assert_eq!(published.snapshot().steps.len(), 10_001);
        assert!(!published.snapshot().is_complete());
    }
}
