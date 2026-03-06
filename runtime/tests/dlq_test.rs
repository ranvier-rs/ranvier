use ranvier_core::event::{DlqPolicy, DlqSink};
use ranvier_core::prelude::*;
use ranvier_runtime::Axon;
use std::sync::Arc;

/// In-memory DLQ sink for integration testing (replaces removed ranvier-observe FileDlqSink).
#[derive(Clone)]
struct MemoryDlqSink {
    letters: Arc<tokio::sync::Mutex<Vec<String>>>,
}

impl MemoryDlqSink {
    fn new() -> Self {
        Self {
            letters: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        }
    }
}

#[async_trait::async_trait]
impl DlqSink for MemoryDlqSink {
    async fn store_dead_letter(
        &self,
        workflow_id: &str,
        circuit_label: &str,
        node_id: &str,
        error_msg: &str,
        _payload: &[u8],
    ) -> Result<(), String> {
        let entry = format!("{workflow_id}|{circuit_label}|{node_id}|{error_msg}");
        self.letters.lock().await.push(entry);
        Ok(())
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum FaultError {
    ExpectedFault,
}

#[derive(Clone)]
struct FaultyStep;

#[async_trait::async_trait]
impl Transition<(), ()> for FaultyStep {
    type Error = FaultError;
    type Resources = ();

    async fn run(
        &self,
        _state: (),
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<(), Self::Error> {
        Outcome::fault(FaultError::ExpectedFault)
    }
}

#[tokio::test]
async fn test_dlq_captures_fault() {
    let dlq_sink = MemoryDlqSink::new();

    let axon = Axon::<(), (), FaultError, ()>::new("DlqTest")
        .with_dlq_sink(dlq_sink.clone())
        .with_dlq_policy(DlqPolicy::SendToDlq)
        .then(FaultyStep);

    let mut bus = Bus::new();
    let outcome = axon.execute((), &(), &mut bus).await;

    // Verify outcome is fault
    match outcome {
        Outcome::Fault(FaultError::ExpectedFault) => {}
        _ => panic!("Expected fault, got {:?}", outcome),
    }

    // Verify DLQ captured the fault
    let letters = dlq_sink.letters.lock().await;
    assert_eq!(letters.len(), 1, "Expected exactly one DLQ entry");
    assert!(
        letters[0].contains("ExpectedFault"),
        "DLQ entry should contain error: {}",
        letters[0]
    );
    assert!(
        letters[0].contains("DlqTest"),
        "DLQ entry should contain circuit label: {}",
        letters[0]
    );
}
