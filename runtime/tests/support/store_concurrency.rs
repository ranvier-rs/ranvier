//! P2-4 adapter-common concurrency fixture (D-CONC-DESIGN C7, cases F1-F10).
//!
//! Every case takes a `connect` function so races run through independent
//! store clients. Races start from a barrier; no case relies on sleeping.
#![allow(dead_code)]

use ranvier_core::{Bus, Outcome, Transition};
use ranvier_runtime::persistence::*;
use ranvier_runtime::{Axon, ExecutionReport, PersistenceOperation};
use std::future::Future;
use std::pin::Pin;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::Barrier;
use tokio::task::JoinSet;

pub type StoreRef = Arc<dyn PersistenceStore>;
pub type Connect = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = StoreRef> + Send>> + Send + Sync>;

const CIRCUIT: &str = "ConcurrencyFixture";

pub struct Fixture {
    connect: Connect,
    label: &'static str,
    run_id: String,
}

fn event(
    trace_id: &str,
    sequence: u64,
    step: u64,
    event_type: PersistenceEventType,
    node_id: Option<String>,
    outcome_kind: &str,
) -> PersistenceEnvelope {
    PersistenceEnvelope {
        trace_id: trace_id.into(),
        circuit: CIRCUIT.into(),
        schematic_version: "1".into(),
        event_sequence: sequence,
        event_type,
        step,
        node_id,
        outcome_kind: outcome_kind.into(),
        timestamp_ms: 1_700_000_000_000 + sequence,
        payload_hash: None,
        payload: None,
    }
}

fn enter(trace_id: &str, sequence: u64) -> PersistenceEnvelope {
    event(
        trace_id,
        sequence,
        0,
        PersistenceEventType::Enter,
        None,
        "Enter",
    )
}

fn checkpoint(trace_id: &str, sequence: u64, tag: impl std::fmt::Display) -> PersistenceEnvelope {
    event(
        trace_id,
        sequence,
        sequence,
        PersistenceEventType::Checkpoint,
        Some(format!("node-{tag}")),
        "Checkpoint",
    )
}

fn terminal(trace_id: &str, sequence: u64) -> PersistenceEnvelope {
    event(
        trace_id,
        sequence,
        sequence,
        PersistenceEventType::Terminal,
        None,
        "Success",
    )
}

fn intervention(revision: u64, tag: impl std::fmt::Display) -> Intervention {
    Intervention {
        target_node: format!("target-{tag}"),
        payload_override: Some(serde_json::json!({ "tag": tag.to_string() })),
        timestamp_ms: 1,
        expected_event_sequence: revision,
    }
}

fn conflict_kind(error: &anyhow::Error) -> PersistenceConflictKind {
    error
        .downcast_ref::<PersistenceConflict>()
        .unwrap_or_else(|| panic!("expected PersistenceConflict, got {error:#}"))
        .kind
        .clone()
}

fn is_race_loss(kind: &PersistenceConflictKind) -> bool {
    matches!(
        kind,
        PersistenceConflictKind::StaleRevision { .. } | PersistenceConflictKind::ConcurrentUpdate
    )
}

#[derive(Clone)]
struct AddOne(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Transition<u32, u32> for AddOne {
    type Error = String;
    type Resources = ();

    async fn run(&self, input: u32, _: &(), _: &mut Bus) -> Outcome<u32, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::Next(input + 1)
    }
}

struct Graph {
    axon: Axon<u32, u32, String>,
    calls: Vec<Arc<AtomicUsize>>,
}

fn graph() -> Graph {
    let calls: Vec<_> = (0..3).map(|_| Arc::new(AtomicUsize::new(0))).collect();
    let axon = Axon::<u32, u32, String>::new(CIRCUIT)
        .then(AddOne(calls[0].clone()))
        .then(AddOne(calls[1].clone()))
        .then(AddOne(calls[2].clone()));
    Graph { axon, calls }
}

impl Graph {
    fn counts(&self) -> Vec<usize> {
        self.calls
            .iter()
            .map(|c| c.load(Ordering::SeqCst))
            .collect()
    }

    fn node(&self, index: usize) -> String {
        self.axon.schematic().nodes[index].id.clone()
    }

    fn event(
        &self,
        trace_id: &str,
        sequence: u64,
        step: u64,
        event_type: PersistenceEventType,
        node: Option<usize>,
        kind: &str,
        payload: Option<serde_json::Value>,
    ) -> PersistenceEnvelope {
        let schematic = self.axon.schematic();
        PersistenceEnvelope {
            trace_id: trace_id.into(),
            circuit: schematic.name.clone(),
            schematic_version: schematic.schema_version.clone(),
            event_sequence: sequence,
            event_type,
            step,
            node_id: node.map(|index| self.node(index)),
            outcome_kind: kind.into(),
            timestamp_ms: 0,
            payload_hash: None,
            payload,
        }
    }
}

async fn run(store: StoreRef, trace_id: &str, graph: &Graph, input: u32) -> Report {
    let mut bus = Bus::new();
    bus.insert(PersistenceTraceId::new(trace_id));
    bus.insert(PersistenceHandle::from_arc(store));
    graph.axon.execute(input, &(), &mut bus).await
}

type Report = ExecutionReport<Outcome<u32, String>>;

fn next_value(report: &Report) -> Option<u32> {
    match report {
        ExecutionReport::Recorded {
            result: Outcome::Next(value),
            ..
        } => Some(*value),
        _ => None,
    }
}

fn label(report: &Report) -> String {
    match report {
        ExecutionReport::Recorded { result, .. } => format!("recorded:{result:?}"),
        ExecutionReport::BlockedBeforeEffect { operation, .. } => format!("blocked:{operation:?}"),
        ExecutionReport::NotRequested { result, .. } => format!("not-requested:{result:?}"),
        ExecutionReport::Unconfirmed { .. } => "unconfirmed".into(),
        ExecutionReport::Unpersisted { .. } => "unpersisted".into(),
    }
}

impl Fixture {
    pub fn new(label: &'static str, connect: Connect) -> Self {
        Self {
            connect,
            label,
            run_id: uuid::Uuid::new_v4().simple().to_string(),
        }
    }

    fn trace(&self, case: &str) -> String {
        format!("conc-{case}-{}", self.run_id)
    }

    async fn store(&self) -> StoreRef {
        (self.connect)().await
    }

    /// Run every case and print one summary line per case.
    pub async fn run_all(&self, rounds: usize) {
        self.append_race(8).await;
        self.append_vs_complete(rounds).await;
        self.append_vs_intervention(rounds).await;
        self.intervention_race(rounds).await;
        self.duplicate_envelope().await;
        self.stale_writer().await;
        self.terminal_monotonic().await;
        self.executor_race(rounds).await;
        self.jump_is_not_reapplied().await;
        self.restarted_writer_sees_pending_intent().await;
    }

    /// F1: N writers append the same sequence; exactly one is acknowledged.
    pub async fn append_race(&self, writers: usize) {
        let id = self.trace("append-race");
        let observer = self.store().await;
        observer.append(enter(&id, 0)).await.unwrap();
        let barrier = Arc::new(Barrier::new(writers));
        let mut tasks = JoinSet::new();
        for index in 0..writers {
            let store = self.store().await;
            let barrier = barrier.clone();
            let id = id.clone();
            tasks.spawn(async move {
                barrier.wait().await;
                (index, store.append(checkpoint(&id, 1, index)).await)
            });
        }
        let mut winners = Vec::new();
        while let Some(joined) = tasks.join_next().await {
            let (index, result) = joined.unwrap();
            match result {
                Ok(()) => winners.push(index),
                Err(error) => {
                    let kind = conflict_kind(&error);
                    assert!(is_race_loss(&kind), "{kind:?}");
                }
            }
        }
        assert_eq!(winners.len(), 1, "exactly one acknowledged append");
        let loaded = observer.load(&id).await.unwrap().unwrap();
        assert_eq!(
            loaded.events,
            vec![enter(&id, 0), checkpoint(&id, 1, winners[0])]
        );
        println!(
            "CONCURRENCY store={} case=F1 writers={writers} acknowledged=1 conflicts={} PASS",
            self.label,
            writers - 1
        );
    }

    /// F2: a terminal append races completion; only the legal order is stored.
    pub async fn append_vs_complete(&self, rounds: usize) {
        let mut completed_in_race = 0;
        for round in 0..rounds {
            let id = self.trace(&format!("append-complete-{round}"));
            let observer = self.store().await;
            observer.append(enter(&id, 0)).await.unwrap();
            let barrier = Arc::new(Barrier::new(2));
            let (appender, completer) = (self.store().await, self.store().await);
            let (b1, b2, id1, id2) = (barrier.clone(), barrier, id.clone(), id.clone());
            let append = tokio::spawn(async move {
                b1.wait().await;
                appender.append(terminal(&id1, 1)).await
            });
            let complete = tokio::spawn(async move {
                b2.wait().await;
                completer.complete(&id2, CompletionState::Success).await
            });
            let (append, complete) = (append.await.unwrap(), complete.await.unwrap());
            append.expect("completion never blocks the terminal append");
            let loaded = observer.load(&id).await.unwrap().unwrap();
            assert_eq!(loaded.events, vec![enter(&id, 0), terminal(&id, 1)]);
            if complete.is_ok() {
                completed_in_race += 1;
                assert_eq!(loaded.completion, Some(CompletionState::Success));
            } else {
                assert_eq!(loaded.completion, None, "no completion without terminal");
            }
            observer
                .complete(&id, CompletionState::Success)
                .await
                .unwrap();
            let late = observer.append(checkpoint(&id, 2, "late")).await;
            assert!(matches!(
                conflict_kind(&late.unwrap_err()),
                PersistenceConflictKind::Completed | PersistenceConflictKind::Terminal
            ));
            assert_eq!(
                observer.load(&id).await.unwrap().unwrap().events.len(),
                2,
                "late append left no event"
            );
        }
        println!(
            "CONCURRENCY store={} case=F2 rounds={rounds} completed_in_race={completed_in_race} PASS",
            self.label
        );
    }

    /// F3: an append races an intervention on the same revision. No write is
    /// lost; an intervention overtaken by an append is kept but never applies.
    pub async fn append_vs_intervention(&self, rounds: usize) {
        let (mut both, mut append_only, mut intervention_only) = (0, 0, 0);
        for round in 0..rounds {
            let id = self.trace(&format!("append-intervention-{round}"));
            let observer = self.store().await;
            observer.append(enter(&id, 0)).await.unwrap();
            let barrier = Arc::new(Barrier::new(2));
            let (appender, editor) = (self.store().await, self.store().await);
            let (b1, b2, id1, id2) = (barrier.clone(), barrier, id.clone(), id.clone());
            let append = tokio::spawn(async move {
                b1.wait().await;
                appender.append(checkpoint(&id1, 1, "writer")).await
            });
            let edit = tokio::spawn(async move {
                b2.wait().await;
                editor
                    .save_intervention(&id2, intervention(1, "race"))
                    .await
            });
            let (append, edit) = (append.await.unwrap(), edit.await.unwrap());
            for error in [append.as_ref().err(), edit.as_ref().err()]
                .into_iter()
                .flatten()
            {
                let kind = conflict_kind(error);
                assert!(is_race_loss(&kind), "{kind:?}");
            }
            let loaded = observer.load(&id).await.unwrap().unwrap();
            let expected_events = if append.is_ok() {
                vec![enter(&id, 0), checkpoint(&id, 1, "writer")]
            } else {
                vec![enter(&id, 0)]
            };
            assert_eq!(loaded.events, expected_events);
            let expected_interventions = if edit.is_ok() {
                vec![intervention(1, "race")]
            } else {
                Vec::new()
            };
            assert_eq!(loaded.interventions, expected_interventions);
            match (append.is_ok(), edit.is_ok()) {
                (true, true) => {
                    // Saved first, then overtaken: bound to revision 1, now at 2.
                    both += 1;
                    assert_eq!(loaded.events.len(), 2);
                }
                (true, false) => append_only += 1,
                (false, true) => intervention_only += 1,
                (false, false) => panic!("at least one writer must win"),
            }
        }
        println!(
            "CONCURRENCY store={} case=F3 rounds={rounds} both={both} append_only={append_only} intervention_only={intervention_only} PASS",
            self.label
        );
    }

    /// F4: two interventions on the same revision; exactly one is stored.
    pub async fn intervention_race(&self, rounds: usize) {
        for round in 0..rounds {
            let id = self.trace(&format!("intervention-race-{round}"));
            let observer = self.store().await;
            observer.append(enter(&id, 0)).await.unwrap();
            let barrier = Arc::new(Barrier::new(2));
            let mut tasks = JoinSet::new();
            for tag in ["left", "right"] {
                let store = self.store().await;
                let barrier = barrier.clone();
                let id = id.clone();
                tasks.spawn(async move {
                    barrier.wait().await;
                    (
                        tag,
                        store.save_intervention(&id, intervention(1, tag)).await,
                    )
                });
            }
            let mut winners = Vec::new();
            while let Some(joined) = tasks.join_next().await {
                let (tag, result) = joined.unwrap();
                match result {
                    Ok(()) => winners.push(tag),
                    Err(error) => assert!(matches!(
                        conflict_kind(&error),
                        PersistenceConflictKind::InterventionExists { revision: 1 }
                            | PersistenceConflictKind::ConcurrentUpdate
                    )),
                }
            }
            assert_eq!(winners.len(), 1);
            let loaded = observer.load(&id).await.unwrap().unwrap();
            assert_eq!(loaded.interventions, vec![intervention(1, winners[0])]);
        }
        println!(
            "CONCURRENCY store={} case=F4 rounds={rounds} stored=1 PASS",
            self.label
        );
    }

    /// F5: rewriting an acknowledged envelope is a conflict, not idempotent.
    pub async fn duplicate_envelope(&self) {
        let id = self.trace("duplicate");
        let store = self.store().await;
        store.append(enter(&id, 0)).await.unwrap();
        store.append(checkpoint(&id, 1, "once")).await.unwrap();
        let before = store.load(&id).await.unwrap().unwrap();
        for duplicate in [checkpoint(&id, 1, "once"), enter(&id, 0)] {
            let sequence = duplicate.event_sequence;
            let error = store.append(duplicate).await.unwrap_err();
            assert_eq!(
                conflict_kind(&error),
                PersistenceConflictKind::StaleRevision {
                    expected: sequence,
                    actual: 2
                }
            );
        }
        assert_eq!(store.load(&id).await.unwrap().unwrap(), before);
        println!("CONCURRENCY store={} case=F5 PASS", self.label);
    }

    /// F6: a writer that read an older revision is rejected for every write.
    pub async fn stale_writer(&self) {
        let id = self.trace("stale");
        let current = self.store().await;
        current.append(enter(&id, 0)).await.unwrap();
        let stale = self.store().await;
        let view = stale.load(&id).await.unwrap().unwrap();
        let revision = view.events.len() as u64;
        current.append(checkpoint(&id, 1, "current")).await.unwrap();
        let after = current.load(&id).await.unwrap().unwrap();
        let expected = PersistenceConflictKind::StaleRevision {
            expected: revision,
            actual: 2,
        };
        let append = stale.append(checkpoint(&id, revision, "stale")).await;
        assert_eq!(conflict_kind(&append.unwrap_err()), expected);
        let edit = stale
            .save_intervention(&id, intervention(revision, "stale"))
            .await;
        assert_eq!(conflict_kind(&edit.unwrap_err()), expected);
        assert_eq!(current.load(&id).await.unwrap().unwrap(), after);
        println!("CONCURRENCY store={} case=F6 PASS", self.label);
    }

    /// F7: terminal and completion are final; every later write is refused.
    pub async fn terminal_monotonic(&self) {
        let id = self.trace("terminal");
        let store = self.store().await;
        store.append(enter(&id, 0)).await.unwrap();
        store.append(terminal(&id, 1)).await.unwrap();
        let edit = store.save_intervention(&id, intervention(2, "late")).await;
        assert_eq!(
            conflict_kind(&edit.unwrap_err()),
            PersistenceConflictKind::Terminal
        );
        store.complete(&id, CompletionState::Success).await.unwrap();
        let completed = store.load(&id).await.unwrap().unwrap();
        let completed_conflict = |error: anyhow::Error| {
            assert_eq!(conflict_kind(&error), PersistenceConflictKind::Completed);
        };
        completed_conflict(store.append(checkpoint(&id, 2, "late")).await.unwrap_err());
        completed_conflict(
            store
                .save_intervention(&id, intervention(2, "late"))
                .await
                .unwrap_err(),
        );
        completed_conflict(store.resume(&id, 1).await.unwrap_err());
        completed_conflict(
            store
                .complete(&id, CompletionState::Fault)
                .await
                .unwrap_err(),
        );
        store.complete(&id, CompletionState::Success).await.unwrap();
        assert_eq!(store.load(&id).await.unwrap().unwrap(), completed);
        println!("CONCURRENCY store={} case=F7 PASS", self.label);
    }

    /// F8: two executors run the same trace. Each step's effect runs exactly
    /// once, no effect is left unconfirmed, and the trace completes.
    pub async fn executor_race(&self, rounds: usize) {
        let mut outcomes = std::collections::BTreeMap::<String, usize>::new();
        for round in 0..rounds {
            let id = self.trace(&format!("executors-{round}"));
            let graph = Arc::new(graph());
            let barrier = Arc::new(Barrier::new(2));
            let mut tasks = JoinSet::new();
            for _ in 0..2 {
                let store = self.store().await;
                let (graph, barrier, id) = (graph.clone(), barrier.clone(), id.clone());
                tasks.spawn(async move {
                    barrier.wait().await;
                    run(store, &id, &graph, 1).await
                });
            }
            let mut reports = Vec::new();
            while let Some(joined) = tasks.join_next().await {
                reports.push(joined.unwrap());
            }
            assert_eq!(graph.counts(), vec![1, 1, 1], "round {round}");
            assert!(
                reports.iter().any(|report| next_value(report) == Some(4)),
                "{:?}",
                reports.iter().map(label).collect::<Vec<_>>()
            );
            for report in &reports {
                assert!(
                    next_value(report) == Some(4)
                        || matches!(
                            report,
                            ExecutionReport::BlockedBeforeEffect { .. }
                                | ExecutionReport::NotRequested { .. }
                        ),
                    "{}",
                    label(report)
                );
                *outcomes.entry(label(report)).or_default() += 1;
            }
            let loaded = self.store().await.load(&id).await.unwrap().unwrap();
            assert_eq!(loaded.completion, Some(CompletionState::Success));
        }
        println!(
            "CONCURRENCY store={} case=F8 rounds={rounds} outcomes={outcomes:?} PASS",
            self.label
        );
    }

    /// F9: a jump that already ran is not applied again after a crash.
    pub async fn jump_is_not_reapplied(&self) {
        let id = self.trace("jump-once");
        let graph = graph();
        let store = self.store().await;
        let seed = |sequence, step, kind, node, event_kind, payload| {
            graph.event(&id, sequence, step, kind, node, event_kind, payload)
        };
        use PersistenceEventType::{Enter, Intent, Result};
        for event in [
            seed(
                0,
                0,
                Enter,
                None,
                "Enter",
                Some(serde_json::json!({"input": 1})),
            ),
            seed(1, 1, Intent, Some(1), "Intent", Some(serde_json::json!(1))),
            seed(
                2,
                1,
                Result,
                Some(1),
                "Next",
                Some(serde_json::json!({"Next": 2})),
            ),
        ] {
            store.append(event).await.unwrap();
        }
        store
            .save_intervention(
                &id,
                Intervention {
                    target_node: graph.node(2),
                    payload_override: Some(serde_json::json!(100)),
                    timestamp_ms: 1,
                    expected_event_sequence: 3,
                },
            )
            .await
            .unwrap();
        // The jump run executed node 2 from 100, then the process stopped.
        for event in [
            seed(3, 2, Enter, None, "Enter", None),
            seed(
                4,
                2,
                Intent,
                Some(2),
                "Intent",
                Some(serde_json::json!(100)),
            ),
            seed(
                5,
                2,
                Result,
                Some(2),
                "Next",
                Some(serde_json::json!({"Next": 101})),
            ),
        ] {
            store.append(event).await.unwrap();
        }
        let report = run(self.store().await, &id, &graph, 1).await;
        assert_eq!(next_value(&report), Some(102), "{}", label(&report));
        assert_eq!(graph.counts(), vec![0, 0, 1]);
        println!("CONCURRENCY store={} case=F9 PASS", self.label);
    }

    /// F10: a writer stopped after its intent was acknowledged; a restarted
    /// writer is blocked by the open intent and changes nothing.
    pub async fn restarted_writer_sees_pending_intent(&self) {
        let id = self.trace("restart");
        let graph = graph();
        let store = self.store().await;
        store
            .append(graph.event(
                &id,
                0,
                0,
                PersistenceEventType::Enter,
                None,
                "Enter",
                Some(serde_json::json!({"input": 1})),
            ))
            .await
            .unwrap();
        store
            .append(graph.event(
                &id,
                1,
                1,
                PersistenceEventType::Intent,
                Some(1),
                "Intent",
                Some(serde_json::json!(1)),
            ))
            .await
            .unwrap();
        let before = store.load(&id).await.unwrap().unwrap();
        let report = run(self.store().await, &id, &graph, 1).await;
        assert!(
            matches!(
                report,
                ExecutionReport::BlockedBeforeEffect {
                    operation: PersistenceOperation::PendingIntent,
                    ..
                }
            ),
            "{}",
            label(&report)
        );
        assert_eq!(graph.counts(), vec![0, 0, 0]);
        assert_eq!(store.load(&id).await.unwrap().unwrap(), before);
        println!("CONCURRENCY store={} case=F10 PASS", self.label);
    }
}
