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
                            value: value.collect_report().await,
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
                        assert_eq!(
                            value.terminal,
                            ranvier_runtime::StreamConsumptionTerminal::Completed
                        );
                        assert_eq!(value.items.len(), 1);
                        assert_eq!(*value.items[0].0, 42);
                        assert_eq!(value.items[0].1.get(), 0);
                        assert_eq!(
                            &*value.items[0].0 as *const usize as usize,
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
async fn configured_store_journals_failed_initializer_as_one_stream_effect() {
    let initialized = Arc::new(AtomicUsize::new(0));
    let backing = InMemoryPersistenceStore::new();
    let mut bus = Bus::new();
    bus.insert(PersistenceHandle::from_store(backing.clone()));
    bus.insert(PersistenceTraceId::new("prefix-only"));
    let report = Axon::<usize, usize, String>::start("prefix-only")
        .then_stream(FailInit(initialized.clone()))
        .execute(42, &(), &mut bus)
        .await;
    assert!(matches!(
        &report,
        StreamInitializationReport::Failed {
            prefix: ExecutionReport::Recorded {
                completion: ranvier_runtime::CompletionConfirmation::Confirmed,
                ..
            },
            initialization: EffectBoundary::MayHaveRun,
            ..
        }
    ));
    assert_eq!(initialized.load(Ordering::SeqCst), 1);
    let persisted = backing
        .load("prefix-only")
        .await
        .unwrap()
        .expect("the stream effect is persisted");
    assert_eq!(persisted.completion, Some(CompletionState::Fault));
    assert_eq!(
        persisted
            .events
            .iter()
            .map(|event| (event.event_type, event.outcome_kind.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (PersistenceEventType::Enter, "Enter"),
            (PersistenceEventType::Intent, "Intent"),
            (PersistenceEventType::Result, "StreamInitFailed"),
            (PersistenceEventType::Terminal, "Fault")
        ]
    );
    // The Intent carries the prefix output; no private initializer detail is stored.
    assert_eq!(persisted.events[1].payload, Some(serde_json::json!(42)));
    assert!(
        !serde_json::to_string(&persisted)
            .unwrap()
            .contains("owned-private-init-detail")
    );
}

#[derive(Clone)]
struct PartialInit(Arc<AtomicUsize>, Option<ranvier_core::CancellationToken>);
#[async_trait::async_trait]
impl StreamingTransition<()> for PartialInit {
    type Item = std::cell::Cell<usize>;
    type Error = String;
    type Resources = ();
    async fn run_stream(
        &self,
        _: (),
        _: &(),
        _: &mut Bus,
    ) -> Result<Pin<Box<dyn futures_util::Stream<Item = Self::Item> + Send>>, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        let token = self.1.clone();
        let mut first = true;
        Ok(Box::pin(futures_util::stream::poll_fn(move |_| {
            if first {
                first = false;
                return std::task::Poll::Ready(Some(std::cell::Cell::new(7)));
            }
            if let Some(token) = &token {
                token.cancel(ranvier_core::CancellationReason::Explicit);
            }
            std::task::Poll::Pending
        })))
    }
}
#[tokio::test]
async fn public_collector_preserves_partial_timeout_cancel_and_precancel_rejection() {
    use ranvier_runtime::StreamConsumptionTerminal;
    for mode in 0..3 {
        let init = Arc::new(AtomicUsize::new(0));
        let token = ranvier_core::CancellationToken::new();
        if mode == 2 {
            token.cancel(ranvier_core::CancellationReason::Explicit);
        }
        let circuit = Axon::<(), (), String>::start("partial-collector")
            .then_stream(PartialInit(
                init.clone(),
                (mode == 1).then_some(token.clone()),
            ))
            .with_timeout(ranvier_core::streaming::StreamTimeoutConfig {
                init: None,
                idle: Some(std::time::Duration::from_millis(20)),
                total: None,
            })
            .collect_into_vec();
        let mut bus = Bus::new();
        let report = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            if mode == 0 {
                circuit.execute((), &(), &mut bus).await
            } else {
                circuit.execute_cancellable((), &(), &mut bus, token).await
            }
        })
        .await
        .unwrap();
        if mode == 2 {
            assert!(matches!(
                report,
                StreamInitializationReport::Rejected {
                    prefix: ExecutionReport::Unpersisted {
                        result: ExecutionTerminal::Cancelled(_),
                        cleanup: None
                    }
                }
            ));
            assert_eq!(init.load(Ordering::SeqCst), 0);
        } else {
            match report {
                StreamInitializationReport::Ready {
                    value,
                    prefix:
                        ExecutionReport::Unpersisted {
                            result: (),
                            cleanup: None,
                        },
                } => {
                    assert_eq!(value.items.len(), 1);
                    assert_eq!(value.items[0].get(), 7);
                    if mode == 0 {
                        assert_eq!(
                            value.terminal,
                            StreamConsumptionTerminal::TimedOut(
                                ranvier_runtime::StreamTimeoutKind::Idle
                            )
                        );
                    } else {
                        assert!(
                            matches!(value.terminal, StreamConsumptionTerminal::Cancelled(context) if context.reason == ranvier_core::CancellationReason::Explicit)
                        );
                    }
                }
                _ => panic!("partial collection lost its typed report"),
            }
            assert_eq!(init.load(Ordering::SeqCst), 1);
        }
    }
}
