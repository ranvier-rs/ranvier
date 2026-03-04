use ranvier_core::event::DlqPolicy;
use ranvier_core::prelude::*;
use ranvier_observe::FileDlqSink;
use ranvier_runtime::Axon;
use tempfile::tempdir;

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
    let dir = tempdir().unwrap();
    let dlq_path = dir.path().to_owned();
    let dlq_sink = FileDlqSink::new(&dlq_path).await.unwrap();

    let axon = Axon::<(), (), FaultError, ()>::new("DlqTest")
        .with_dlq_sink(dlq_sink)
        .with_dlq_policy(DlqPolicy::SendToDlq)
        .then(FaultyStep);

    let mut bus = Bus::new();
    let outcome = axon.execute((), &(), &mut bus).await;

    // Verify outcome is fault
    match outcome {
        Outcome::Fault(FaultError::ExpectedFault) => {}
        _ => panic!("Expected fault, got {:?}", outcome),
    }

    // Check if DLQ file was created and contains the fault
    let mut files = std::fs::read_dir(&dlq_path).unwrap();
    let entry = files
        .next()
        .expect("Expected at least one DLQ file")
        .unwrap();
    let content = std::fs::read_to_string(entry.path()).unwrap();

    assert!(content.contains("ExpectedFault"));
    assert!(content.contains("DlqTest"));
}
