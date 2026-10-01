#![cfg(feature = "streaming")]
use ranvier_core::{Bus, Outcome, Transition, streaming::StreamingTransition};
use ranvier_runtime::{
    Axon, EffectBoundary, ExecutionReport, ExecutionTerminal, PersistenceOperation,
    StreamInitializationReport, persistence::*,
};
use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
#[derive(serde::Deserialize)]
struct Owned(Box<usize>);
impl serde::Serialize for Owned {
    fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom(
            "synthetic output encoding failure",
        ))
    }
}
// Intentionally neither Serialize, Clone nor Sync: item constraints remain Send only.
struct Item(Box<usize>, std::cell::Cell<usize>);
#[derive(Clone)]
struct Produce {
    effects: Arc<AtomicUsize>,
    address: Arc<AtomicUsize>,
}
#[async_trait::async_trait]
impl Transition<usize, Owned> for Produce {
    type Error = String;
    type Resources = ();
    async fn run(&self, input: usize, _: &(), _: &mut Bus) -> Outcome<Owned, String> {
        self.effects.fetch_add(1, Ordering::SeqCst);
        let value = Box::new(input);
        self.address
            .store(&*value as *const usize as usize, Ordering::SeqCst);
        Outcome::Next(Owned(value))
    }
}
#[derive(Clone)]
struct Init(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl StreamingTransition<Owned> for Init {
    type Item = Item;
    type Error = String;
    type Resources = ();
    async fn run_stream(
        &self,
        value: Owned,
        _: &(),
        _: &mut Bus,
    ) -> Result<Pin<Box<dyn futures_util::Stream<Item = Item> + Send>>, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Box::pin(futures_util::stream::iter(vec![Item(
            value.0,
            std::cell::Cell::new(0),
        )])))
    }
}
#[tokio::test]
async fn public_mapping_and_collection_preserve_move_only_rejection_without_initializing() {
    use futures_util::StreamExt;
    for stored in [false, true] {
        for wrapper in 0..3 {
            let effects = Arc::new(AtomicUsize::new(0));
            let initialized = Arc::new(AtomicUsize::new(0));
            let mapped = Arc::new(AtomicUsize::new(0));
            let address = Arc::new(AtomicUsize::new(0));
            let mut circuit = Axon::<usize, usize, String>::start("public-stream-report")
                .then(Produce {
                    effects: effects.clone(),
                    address: address.clone(),
                })
                .then_stream(Init(initialized.clone()));
            if wrapper == 1 {
                let mapped = mapped.clone();
                circuit = circuit.map_items(move |item| {
                    mapped.fetch_add(1, Ordering::SeqCst);
                    item
                });
            }
            let mut bus = Bus::new();
            if stored {
                bus.insert(PersistenceHandle::from_store(
                    InMemoryPersistenceStore::new(),
                ));
            }
            let report = if wrapper == 2 {
                circuit.collect_into_vec().execute(42, &(), &mut bus).await
            } else {
                let report = circuit
                    .execute_cancellable(42, &(), &mut bus, ranvier_core::CancellationToken::new())
                    .await;
                match report {
                    StreamInitializationReport::Ready { prefix, value } => {
                        StreamInitializationReport::Ready {
                            prefix,
                            value: value.collect::<Vec<_>>().await,
                        }
                    }
                    report => report.map_value(|_| unreachable!("Ready handled above")),
                }
            };
            if stored {
                match report {
                    StreamInitializationReport::Rejected {
                        prefix:
                            ExecutionReport::Unconfirmed {
                                result: ExecutionTerminal::Outcome(Outcome::Next(value)),
                                operation: PersistenceOperation::Result,
                                effect: EffectBoundary::MayHaveRun,
                                ..
                            },
                    } => assert_eq!(
                        &*value.0 as *const usize as usize,
                        address.load(Ordering::SeqCst)
                    ),
                    _ => panic!("encoding failure lost typed known result"),
                }
                assert_eq!(initialized.load(Ordering::SeqCst), 0);
                assert_eq!(mapped.load(Ordering::SeqCst), 0);
            } else {
                match report {
                    StreamInitializationReport::Ready {
                        prefix:
                            ExecutionReport::Unpersisted {
                                result: (),
                                cleanup: None,
                            },
                        value,
                    } => {
                        assert_eq!(value.len(), 1);
                        assert_eq!(*value[0].0, 42);
                        assert_eq!(value[0].1.get(), 0);
                        assert_eq!(
                            &*value[0].0 as *const usize as usize,
                            address.load(Ordering::SeqCst)
                        );
                    }
                    _ => panic!("move-only stream was not initialized"),
                }
                assert_eq!(initialized.load(Ordering::SeqCst), 1);
                assert_eq!(mapped.load(Ordering::SeqCst), usize::from(wrapper == 1));
            }
            assert_eq!(effects.load(Ordering::SeqCst), 1);
        }
    }
}

#[derive(Clone)]
struct FailInit(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl StreamingTransition<usize> for FailInit {
    type Item = ();
    type Error = String;
    type Resources = ();
    async fn run_stream(
        &self,
        _: usize,
        _: &(),
        _: &mut Bus,
    ) -> Result<Pin<Box<dyn futures_util::Stream<Item = ()> + Send>>, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err("owned-private-init-detail".into())
    }
}
#[tokio::test]
async fn acknowledged_prefix_completion_does_not_certify_failed_initializer() {
    let initialized = Arc::new(AtomicUsize::new(0));
    let backing = InMemoryPersistenceStore::new();
    let mut bus = Bus::new();
    bus.insert(PersistenceHandle::from_store(backing.clone()));
    bus.insert(PersistenceTraceId::new("prefix-only"));
    let report = Axon::<usize, usize, String>::start("prefix-only")
        .then_stream(FailInit(initialized.clone()))
        .execute(42, &(), &mut bus)
        .await;
    assert!(matches!(report, StreamInitializationReport::Failed {
        prefix: ExecutionReport::Recorded { result: (), completion: ranvier_runtime::CompletionConfirmation::Confirmed, .. },
        error: ranvier_runtime::StreamInitializationError::Transition(ref detail),
        initialization: EffectBoundary::MayHaveRun,
    } if detail == "\"owned-private-init-detail\""));
    assert_eq!(initialized.load(Ordering::SeqCst), 1);
    // This is deliberately recorded as prefix evidence, not accepted as a completed
    // full streaming graph by this assertion. Consumers must distinguish phases.
    assert_eq!(
        backing
            .load("prefix-only")
            .await
            .unwrap()
            .unwrap()
            .completion,
        Some(CompletionState::Success)
    );
}
