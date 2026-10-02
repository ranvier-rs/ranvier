use ranvier_core::{Bus, Outcome, Transition, saga::SagaPolicy};
use ranvier_runtime::{
    Axon, ExecutionReport, PersistenceOperation,
    persistence::{InMemoryPersistenceStore, PersistenceHandle},
};
use serde::{Deserialize, Serialize, Serializer};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

const PRIVATE: &str = "private-compensation-input-serializer-sentinel";
#[derive(Clone, Deserialize)]
struct Input {
    panic: bool,
}
impl Serialize for Input {
    fn serialize<S: Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
        if self.panic {
            panic!("{PRIVATE}");
        }
        Err(serde::ser::Error::custom(PRIVATE))
    }
}
#[derive(Clone)]
struct Effect(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl Transition<Input, Input> for Effect {
    type Error = String;
    type Resources = ();
    async fn run(&self, input: Input, _: &(), _: &mut Bus) -> Outcome<Input, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::Next(input)
    }
}
#[derive(Clone)]
struct Undo(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl Transition<Input, ()> for Undo {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: Input, _: &(), _: &mut Bus) -> Outcome<(), String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::Next(())
    }
}
#[tokio::test]
async fn failing_or_panicking_input_encoder_blocks_compensated_effect_before_invocation() {
    for panic in [false, true] {
        for policy in [SagaPolicy::Disabled, SagaPolicy::Enabled] {
            let effects = Arc::new(AtomicUsize::new(0));
            let undos = Arc::new(AtomicUsize::new(0));
            let graph = Axon::<Input, Input, String>::start("encoding-input")
                .with_saga_policy(policy)
                .then_compensated(Effect(effects.clone()), Undo(undos.clone()));
            let mut bus = Bus::new();
            bus.insert(PersistenceHandle::from_store(
                InMemoryPersistenceStore::new(),
            ));
            let report = graph.execute(Input { panic }, &(), &mut bus).await;
            assert!(matches!(
                report,
                ExecutionReport::BlockedBeforeEffect {
                    operation: PersistenceOperation::RecoveryPayload,
                    ..
                }
            ));
            assert_eq!(effects.load(Ordering::SeqCst), 0);
            assert_eq!(undos.load(Ordering::SeqCst), 0);
            assert!(!serde_json::to_string(&report).unwrap().contains(PRIVATE));
        }
    }
}
