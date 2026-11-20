//! STREAM-DURABLE-INTENT-01 (D-STREAM-DESIGN S1-S8): a configured store
//! journals the whole stream as one effect in the prefix's trace.
#![cfg(feature = "streaming")]
use futures_util::StreamExt;
use ranvier_core::{Bus, Outcome, Transition, streaming::StreamingTransition};
use ranvier_runtime::{
    Axon, CompletionConfirmation, EffectBoundary, ExecutionReport, ExecutionTerminal,
    PersistenceOperation, StreamConsumptionTerminal, StreamDurability, StreamInitializationReport,
    persistence::*,
};
use std::{
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

#[derive(Clone)]
struct Effect(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl Transition<usize, usize> for Effect {
    type Error = String;
    type Resources = ();
    async fn run(&self, input: usize, _: &(), _: &mut Bus) -> Outcome<usize, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::Next(input + 1)
    }
}

/// Yields `count` items, then either ends or stays pending forever.
#[derive(Clone)]
struct Chunks {
    calls: Arc<AtomicUsize>,
    count: usize,
    then_pending: bool,
    fail: bool,
}
impl Chunks {
    fn new(count: usize) -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            count,
            then_pending: false,
            fail: false,
        }
    }
}
#[async_trait::async_trait]
impl StreamingTransition<usize> for Chunks {
    type Item = usize;
    type Error = String;
    type Resources = ();
    async fn run_stream(
        &self,
        input: usize,
        _: &(),
        _: &mut Bus,
    ) -> Result<Pin<Box<dyn futures_util::Stream<Item = usize> + Send>>, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err("private-initializer-detail".into());
        }
        let items = futures_util::stream::iter((0..self.count).map(move |i| input + i));
        Ok(if self.then_pending {
            Box::pin(items.chain(futures_util::stream::pending()))
        } else {
            Box::pin(items)
        })
    }
}

/// Store whose selected write commits but loses its acknowledgement.
#[derive(Clone)]
struct LossyStore {
    backing: InMemoryPersistenceStore,
    lose_append: Arc<Mutex<Option<(PersistenceEventType, String)>>>,
    lose_complete: bool,
}
impl LossyStore {
    fn new(lose: Option<(PersistenceEventType, &str)>, lose_complete: bool) -> Self {
        Self {
            backing: InMemoryPersistenceStore::new(),
            lose_append: Arc::new(Mutex::new(lose.map(|(t, k)| (t, k.to_string())))),
            lose_complete,
        }
    }
}
#[async_trait::async_trait]
impl PersistenceStore for LossyStore {
    async fn append(&self, event: PersistenceEnvelope) -> anyhow::Result<()> {
        let lose = self
            .lose_append
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|(t, k)| *t == event.event_type && *k == event.outcome_kind);
        self.backing.append(event).await?;
        if lose {
            anyhow::bail!("synthetic lost append acknowledgement");
        }
        Ok(())
    }
    async fn load(&self, trace: &str) -> anyhow::Result<Option<PersistedTrace>> {
        self.backing.load(trace).await
    }
    async fn resume(&self, trace: &str, step: u64) -> anyhow::Result<ResumeCursor> {
        self.backing.resume(trace, step).await
    }
    async fn complete(&self, trace: &str, completion: CompletionState) -> anyhow::Result<()> {
        self.backing.complete(trace, completion).await?;
        if self.lose_complete {
            anyhow::bail!("synthetic lost completion acknowledgement");
        }
        Ok(())
    }
    async fn save_intervention(
        &self,
        trace: &str,
        intervention: Intervention,
    ) -> anyhow::Result<()> {
        self.backing.save_intervention(trace, intervention).await
    }
}

fn stored_bus(store: Arc<dyn PersistenceStore>, trace: &str, auto_complete: bool) -> Bus {
    let mut bus = Bus::new();
    bus.insert(PersistenceHandle::from_arc(store));
    bus.insert(PersistenceTraceId::new(trace));
    bus.insert(PersistenceAutoComplete(auto_complete));
    bus
}

fn kinds(trace: &PersistedTrace) -> Vec<(PersistenceEventType, u64, String)> {
    trace
        .events
        .iter()
        .map(|event| (event.event_type, event.step, event.outcome_kind.clone()))
        .collect()
}

fn circuit(
    effects: &Arc<AtomicUsize>,
    stream: Chunks,
) -> ranvier_runtime::StreamingAxon<usize, usize, usize, String> {
    Axon::<usize, usize, String>::start("durable-stream")
        .then(Effect(effects.clone()))
        .then_stream(stream)
}

#[tokio::test]
async fn completed_stream_records_intent_result_terminal_and_completion() {
    for auto_complete in [true, false] {
        let store = InMemoryPersistenceStore::new();
        let effects = Arc::new(AtomicUsize::new(0));
        let stream = Chunks::new(3);
        let calls = stream.calls.clone();
        let streaming = circuit(&effects, stream);
        let stream_node = streaming.schematic.nodes.last().unwrap().id.clone();
        let mut bus = stored_bus(Arc::new(store.clone()), "completed", auto_complete);
        let report = streaming
            .collect_into_vec()
            .execute(10, &(), &mut bus)
            .await;
        let StreamInitializationReport::Ready { prefix, value } = report else {
            panic!("stream was not initialized");
        };
        assert!(matches!(
            prefix,
            ExecutionReport::Recorded {
                completion: CompletionConfirmation::NotRequested,
                ..
            }
        ));
        assert_eq!(value.items, vec![11, 12, 13]);
        assert_eq!(value.terminal, StreamConsumptionTerminal::Completed);
        assert_eq!(
            value.durability,
            StreamDurability::Recorded {
                completion: if auto_complete {
                    CompletionConfirmation::Confirmed
                } else {
                    CompletionConfirmation::NotRequested
                }
            }
        );
        let trace = store.load("completed").await.unwrap().unwrap();
        assert_eq!(
            kinds(&trace),
            vec![
                (PersistenceEventType::Enter, 0, "Enter".into()),
                (PersistenceEventType::Intent, 1, "Intent".into()),
                (PersistenceEventType::Result, 1, "Next".into()),
                (PersistenceEventType::Intent, 2, "Intent".into()),
                (PersistenceEventType::Result, 2, "StreamCompleted".into()),
                (PersistenceEventType::Terminal, 3, "Next".into()),
            ]
        );
        // The stream events use the schematic's stream node; items are not journaled.
        assert_eq!(
            trace.events[3].node_id.as_deref(),
            Some(stream_node.as_str())
        );
        assert_eq!(trace.events[3].payload, Some(serde_json::json!(11)));
        assert_eq!(
            trace.events[4].payload,
            Some(serde_json::json!({"terminal": "completed", "items_observed": 3}))
        );
        assert_eq!(
            trace.completion,
            auto_complete.then_some(CompletionState::Success)
        );
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn partial_timeout_and_cancellation_record_their_own_end() {
    for cancel in [false, true] {
        let store = InMemoryPersistenceStore::new();
        let effects = Arc::new(AtomicUsize::new(0));
        let mut stream = Chunks::new(2);
        stream.then_pending = true;
        let token = ranvier_core::CancellationToken::new();
        let streaming =
            circuit(&effects, stream).with_timeout(ranvier_core::streaming::StreamTimeoutConfig {
                init: None,
                idle: Some(std::time::Duration::from_secs(1)),
                total: None,
            });
        let mut bus = stored_bus(Arc::new(store.clone()), "partial", true);
        let StreamInitializationReport::Ready { value, .. } = streaming
            .execute_cancellable(1, &(), &mut bus, token.clone())
            .await
        else {
            panic!("stream was not initialized");
        };
        let mut observed = value;
        assert_eq!(observed.next().await, Some(2));
        assert_eq!(observed.next().await, Some(3));
        if cancel {
            token.cancel(ranvier_core::CancellationReason::Explicit);
        }
        assert_eq!(observed.next().await, None);
        assert_eq!(
            observed.durability(),
            Some(&StreamDurability::Recorded {
                completion: CompletionConfirmation::Confirmed
            })
        );
        let trace = store.load("partial").await.unwrap().unwrap();
        let (result, terminal, completion, label) = if cancel {
            assert!(matches!(
                observed.terminal(),
                Some(StreamConsumptionTerminal::Cancelled(_))
            ));
            (
                "StreamCancelled",
                "Cancelled",
                CompletionState::Cancelled,
                "cancelled",
            )
        } else {
            assert!(matches!(
                observed.terminal(),
                Some(StreamConsumptionTerminal::TimedOut(_))
            ));
            (
                "StreamTimedOut",
                "Fault",
                CompletionState::Fault,
                "timed_out",
            )
        };
        let events = kinds(&trace);
        assert_eq!(events[4], (PersistenceEventType::Result, 2, result.into()));
        assert_eq!(
            events[5],
            (PersistenceEventType::Terminal, 3, terminal.into())
        );
        assert_eq!(
            trace.events[4].payload,
            Some(serde_json::json!({"terminal": label, "items_observed": 2}))
        );
        assert_eq!(trace.completion, Some(completion));
    }
}

#[tokio::test]
async fn dropped_stream_leaves_pending_intent_and_is_never_replayed() {
    let store = InMemoryPersistenceStore::new();
    let effects = Arc::new(AtomicUsize::new(0));
    let mut stream = Chunks::new(1);
    stream.then_pending = true;
    let calls = stream.calls.clone();
    let streaming = circuit(&effects, stream);
    let mut bus = stored_bus(Arc::new(store.clone()), "dropped", true);
    let StreamInitializationReport::Ready { mut value, .. } =
        streaming.execute(5, &(), &mut bus).await
    else {
        panic!("stream was not initialized");
    };
    assert_eq!(value.next().await, Some(6));
    assert!(value.durability().is_none());
    drop(value);
    let trace = store.load("dropped").await.unwrap().unwrap();
    assert_eq!(
        kinds(&trace).last().unwrap(),
        &(PersistenceEventType::Intent, 2, "Intent".to_string())
    );
    assert_eq!(trace.completion, None);

    // A later invocation of the same trace must reconcile, not replay.
    let mut bus = stored_bus(Arc::new(store.clone()), "dropped", true);
    let report = streaming.execute(5, &(), &mut bus).await;
    assert!(matches!(
        report,
        StreamInitializationReport::Rejected {
            prefix: ExecutionReport::BlockedBeforeEffect {
                operation: PersistenceOperation::PendingIntent,
                effect: EffectBoundary::NotInvoked,
                ..
            }
        }
    ));
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn abandon_records_partial_delivery_and_ends_consumption() {
    let store = InMemoryPersistenceStore::new();
    let effects = Arc::new(AtomicUsize::new(0));
    let mut stream = Chunks::new(2);
    stream.then_pending = true;
    let streaming = circuit(&effects, stream);
    let mut bus = stored_bus(Arc::new(store.clone()), "abandoned", true);
    let StreamInitializationReport::Ready { mut value, .. } =
        streaming.execute(0, &(), &mut bus).await
    else {
        panic!("stream was not initialized");
    };
    assert_eq!(value.next().await, Some(1));
    let durability = value.abandon().await;
    assert_eq!(
        durability,
        StreamDurability::Recorded {
            completion: CompletionConfirmation::Confirmed
        }
    );
    assert_eq!(value.abandon().await, durability);
    assert_eq!(value.next().await, None);
    assert!(value.terminal().is_none());
    let trace = store.load("abandoned").await.unwrap().unwrap();
    let events = kinds(&trace);
    assert_eq!(
        events[4],
        (PersistenceEventType::Result, 2, "StreamAbandoned".into())
    );
    assert_eq!(
        events[5],
        (PersistenceEventType::Terminal, 3, "Cancelled".into())
    );
    assert_eq!(
        trace.events[4].payload,
        Some(serde_json::json!({"terminal": "abandoned", "items_observed": 1}))
    );
    assert_eq!(trace.completion, Some(CompletionState::Cancelled));
}

#[tokio::test]
async fn lost_intent_acknowledgement_never_invokes_the_initializer() {
    let store = LossyStore::new(Some((PersistenceEventType::Intent, "Intent")), false);
    // A prefix without steps: the only Intent is the stream's.
    let stream = Chunks::new(1);
    let calls = stream.calls.clone();
    let streaming = Axon::<usize, usize, String>::start("lost-intent").then_stream(stream);
    let mut bus = stored_bus(Arc::new(store.clone()), "lost-intent", true);
    let report = streaming.execute(9, &(), &mut bus).await;
    match report {
        StreamInitializationReport::Rejected {
            prefix:
                ExecutionReport::Unconfirmed {
                    result: ExecutionTerminal::Outcome(Outcome::Next(9)),
                    operation: PersistenceOperation::Intent,
                    effect: EffectBoundary::NotInvoked,
                    ..
                },
        } => {}
        _ => panic!("lost stream Intent was not reported with the known prefix output"),
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let trace = store.backing.load("lost-intent").await.unwrap().unwrap();
    assert!(
        trace
            .events
            .iter()
            .all(|event| event.event_type != PersistenceEventType::Terminal)
    );
    assert_eq!(trace.completion, None);
}

#[tokio::test]
async fn lost_intent_after_prefix_effect_keeps_typed_output() {
    let store = LossyStore::new(None, false);
    let effects = Arc::new(AtomicUsize::new(0));
    let stream = Chunks::new(1);
    let calls = stream.calls.clone();
    let streaming = circuit(&effects, stream);
    // Lose only the second Intent (the stream's), after the prefix effect ran.
    let seen = Arc::new(AtomicUsize::new(0));
    struct SecondIntent(LossyStore, Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl PersistenceStore for SecondIntent {
        async fn append(&self, event: PersistenceEnvelope) -> anyhow::Result<()> {
            let lose = event.event_type == PersistenceEventType::Intent
                && self.1.fetch_add(1, Ordering::SeqCst) == 1;
            self.0.append(event).await?;
            if lose {
                anyhow::bail!("synthetic lost stream Intent acknowledgement");
            }
            Ok(())
        }
        async fn load(&self, trace: &str) -> anyhow::Result<Option<PersistedTrace>> {
            self.0.load(trace).await
        }
        async fn resume(&self, trace: &str, step: u64) -> anyhow::Result<ResumeCursor> {
            self.0.resume(trace, step).await
        }
        async fn complete(&self, trace: &str, c: CompletionState) -> anyhow::Result<()> {
            self.0.complete(trace, c).await
        }
        async fn save_intervention(&self, trace: &str, i: Intervention) -> anyhow::Result<()> {
            self.0.save_intervention(trace, i).await
        }
    }
    let mut bus = stored_bus(
        Arc::new(SecondIntent(store.clone(), seen)),
        "lost-stream-intent",
        true,
    );
    match streaming.execute(1, &(), &mut bus).await {
        StreamInitializationReport::Rejected {
            prefix:
                ExecutionReport::Unconfirmed {
                    result: ExecutionTerminal::Outcome(Outcome::Next(2)),
                    operation: PersistenceOperation::Intent,
                    effect: EffectBoundary::MayHaveRun,
                    ..
                },
        } => {}
        _ => panic!("lost stream Intent lost the typed prefix output"),
    }
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let trace = store
        .backing
        .load("lost-stream-intent")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        kinds(&trace).last().unwrap(),
        &(PersistenceEventType::Intent, 2, "Intent".to_string())
    );
}

#[tokio::test]
async fn lost_end_acknowledgements_are_reported_after_consumption() {
    let cases: [(
        Option<(PersistenceEventType, &str)>,
        bool,
        PersistenceOperation,
    ); 3] = [
        (
            Some((PersistenceEventType::Result, "StreamCompleted")),
            false,
            PersistenceOperation::Result,
        ),
        (
            Some((PersistenceEventType::Terminal, "Next")),
            false,
            PersistenceOperation::Terminal,
        ),
        (None, true, PersistenceOperation::Complete),
    ];
    for (lose, lose_complete, operation) in cases {
        let store = LossyStore::new(lose, lose_complete);
        let effects = Arc::new(AtomicUsize::new(0));
        let streaming = circuit(&effects, Chunks::new(2));
        let mut bus = stored_bus(Arc::new(store.clone()), "lost-end", true);
        let report = streaming.collect_into_vec().execute(0, &(), &mut bus).await;
        let StreamInitializationReport::Ready { value, .. } = report else {
            panic!("stream was not initialized");
        };
        // Consumption completed; its durability is reported separately.
        assert_eq!(value.terminal, StreamConsumptionTerminal::Completed);
        assert_eq!(value.items, vec![1, 2]);
        assert_eq!(
            value.durability,
            StreamDurability::Unconfirmed { operation }
        );
        let trace = store.backing.load("lost-end").await.unwrap().unwrap();
        let has_terminal = trace
            .events
            .iter()
            .any(|event| event.event_type == PersistenceEventType::Terminal);
        // A lost Result stops the Terminal; later losses keep earlier writes.
        assert_eq!(has_terminal, operation != PersistenceOperation::Result);
    }
}

#[tokio::test]
async fn lost_initializer_failure_result_skips_terminal_and_reports_prefix() {
    let store = LossyStore::new(
        Some((PersistenceEventType::Result, "StreamInitFailed")),
        false,
    );
    let effects = Arc::new(AtomicUsize::new(0));
    let mut stream = Chunks::new(0);
    stream.fail = true;
    let streaming = circuit(&effects, stream);
    let mut bus = stored_bus(Arc::new(store.clone()), "lost-init-result", true);
    match streaming.execute(0, &(), &mut bus).await {
        StreamInitializationReport::Failed {
            prefix:
                ExecutionReport::Unconfirmed {
                    operation: PersistenceOperation::Result,
                    effect: EffectBoundary::MayHaveRun,
                    ..
                },
            initialization: EffectBoundary::MayHaveRun,
            initialization_cleanup: None,
            ..
        } => {}
        _ => panic!("lost initializer Result was not reported"),
    }
    let trace = store
        .backing
        .load("lost-init-result")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        kinds(&trace).last().unwrap(),
        &(
            PersistenceEventType::Result,
            2,
            "StreamInitFailed".to_string()
        )
    );
    assert_eq!(trace.completion, None);
}

#[tokio::test]
async fn cancellation_after_prefix_records_only_the_deferred_terminal() {
    let store = InMemoryPersistenceStore::new();
    let effects = Arc::new(AtomicUsize::new(0));
    let stream = Chunks::new(1);
    let calls = stream.calls.clone();
    // The prefix effect cancels the token, so cancellation is observed after
    // the prefix ends in Next and before the initializer.
    #[derive(Clone)]
    struct CancelAfter(Arc<AtomicUsize>, ranvier_core::CancellationToken);
    #[async_trait::async_trait]
    impl Transition<usize, usize> for CancelAfter {
        type Error = String;
        type Resources = ();
        async fn run(&self, input: usize, _: &(), _: &mut Bus) -> Outcome<usize, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            self.1.cancel(ranvier_core::CancellationReason::Explicit);
            Outcome::Next(input)
        }
    }
    let token = ranvier_core::CancellationToken::new();
    let streaming = Axon::<usize, usize, String>::start("cancel-after-prefix")
        .then(CancelAfter(effects.clone(), token.clone()))
        .then_stream(stream);
    let mut bus = stored_bus(Arc::new(store.clone()), "cancel-after-prefix", true);
    let report = streaming.execute_cancellable(3, &(), &mut bus, token).await;
    let trace = store.load("cancel-after-prefix").await.unwrap().unwrap();
    match report {
        StreamInitializationReport::Cancelled {
            initialization: EffectBoundary::NotInvoked,
            prefix: ExecutionReport::Recorded { .. },
            ..
        } => {
            // No stream Intent; the deferred Terminal closes the trace.
            assert!(trace.events.iter().all(|event| !(event.event_type
                == PersistenceEventType::Intent
                && event.step == 2)));
            assert_eq!(
                kinds(&trace).last().unwrap(),
                &(PersistenceEventType::Terminal, 3, "Cancelled".to_string())
            );
            assert_eq!(trace.completion, Some(CompletionState::Cancelled));
        }
        // Cancellation may win inside the prefix instead; then the prefix
        // writes its own cancellation terminal and the stream never starts.
        StreamInitializationReport::Rejected { .. } => {
            assert!(trace.completion.is_some());
        }
        _ => panic!("unexpected report after prefix cancellation"),
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn unconfigured_store_reports_unpersisted_durability() {
    let effects = Arc::new(AtomicUsize::new(0));
    let streaming = circuit(&effects, Chunks::new(1));
    let mut bus = Bus::new();
    let StreamInitializationReport::Ready { prefix, value } =
        streaming.collect_into_vec().execute(0, &(), &mut bus).await
    else {
        panic!("stream was not initialized");
    };
    assert!(matches!(prefix, ExecutionReport::Unpersisted { .. }));
    assert_eq!(value.durability, StreamDurability::Unpersisted);
}
