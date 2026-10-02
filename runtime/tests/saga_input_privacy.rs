use ranvier_core::{
    Bus, Outcome, Transition,
    saga::{SagaPolicy, SagaStack},
};
use ranvier_runtime::{Axon, ExecutionReport, SagaRollbackOutcome};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
const SECRET: &str = "synthetic-private-compensation-input";
#[derive(Clone)]
struct Effect(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl Transition<u32, u32> for Effect {
    type Error = String;
    type Resources = ();
    async fn run(&self, value: u32, _: &(), _: &mut Bus) -> Outcome<u32, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::Next(value)
    }
}
#[derive(Clone)]
struct Undo(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl Transition<u32, ()> for Undo {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: u32, _: &(), _: &mut Bus) -> Outcome<(), String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::Next(())
    }
}
#[derive(Clone)]
struct Corrupt;
#[async_trait::async_trait]
impl Transition<u32, u32> for Corrupt {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: u32, _: &(), bus: &mut Bus) -> Outcome<u32, String> {
        bus.read_mut::<SagaStack>()
            .unwrap()
            .tasks
            .last_mut()
            .unwrap()
            .input_snapshot = serde_json::to_vec(SECRET).unwrap();
        Outcome::Fault("synthetic downstream fault".into())
    }
}
struct Writer(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
#[tokio::test(flavor = "current_thread")]
async fn malformed_compensation_input_redacts_logs_and_payload_and_keeps_rollback_incomplete() {
    let logs = Arc::new(Mutex::new(Vec::new()));
    let writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_target(false)
        .with_writer(move || Writer(writer.clone()))
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let effects = Arc::new(AtomicUsize::new(0));
    let undo = Arc::new(AtomicUsize::new(0));
    let prefix = Axon::<u32, u32, String>::start("compensation-input-privacy")
        .with_saga_policy(SagaPolicy::Enabled)
        .then_compensated(Effect(effects.clone()), Undo(undo.clone()));
    let handler = prefix
        .saga_compensation_registry
        .read()
        .unwrap()
        .handlers
        .values()
        .next()
        .unwrap()
        .clone();
    let result = handler(serde_json::to_vec(SECRET).unwrap(), &(), &mut Bus::new()).await;
    let Outcome::Emit(label, Some(payload)) = result else {
        panic!("invalid snapshot must emit an incomplete classification")
    };
    assert_eq!(label, "saga.compensation.input_deserialization_failed");
    assert!(
        !payload.to_string().contains(SECRET),
        "private decode detail reached emitted payload"
    );
    assert_eq!(undo.load(Ordering::SeqCst), 0);
    let report = prefix.then(Corrupt).execute(7, &(), &mut Bus::new()).await;
    assert!(matches!(
        &report,
        ExecutionReport::Unpersisted {
            result: Outcome::Fault(_),
            ..
        }
    ));
    let cleanup = report.cleanup().unwrap();
    assert!(cleanup.finished);
    assert!(!cleanup.is_complete());
    assert_eq!(cleanup.steps.len(), 1);
    assert_eq!(cleanup.steps[0].outcome, SagaRollbackOutcome::Emitted);
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(undo.load(Ordering::SeqCst), 0);
    let captured = String::from_utf8(logs.lock().unwrap().clone()).unwrap();
    assert!(
        captured.contains("Saga compensation input deserialization failed"),
        "capture must include the actual diagnostic"
    );
    assert!(
        !captured.contains(SECRET),
        "private decode detail reached tracing"
    );
    assert!(!serde_json::to_string(cleanup).unwrap().contains(SECRET));
}
