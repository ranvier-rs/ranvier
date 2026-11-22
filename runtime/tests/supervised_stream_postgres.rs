//! P4-6 live check: a supervised stream (the path HTTP SSE routes use) on a
//! real PostgreSQL trace store. A normal end records StreamCompleted and is
//! released; a consumer disconnect records StreamAbandoned, runs the prefix
//! compensation and is released once its cleanup completed.
//!
//! Run with `RANVIER_PERSISTENCE_POSTGRES_URL` and `--ignored`.
#![cfg(all(feature = "streaming", feature = "persistence-postgres"))]
use ranvier_core::{Bus, CancellationToken, Outcome, Transition, streaming::StreamingTransition};
use ranvier_runtime::Axon;
use ranvier_runtime::persistence::{
    CompletionState, PersistenceEventType, PersistenceHandle, PersistenceStore, PersistenceTraceId,
    PostgresPersistenceStore,
};
use ranvier_runtime::streaming_axon::OwnedStreamContext;
use ranvier_runtime::streaming_supervisor::StreamSupervisor;
use std::pin::Pin;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

#[derive(Clone)]
struct Effect(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl Transition<(), ()> for Effect {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: (), _: &(), _: &mut Bus) -> Outcome<(), String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::Next(())
    }
}
#[derive(Clone)]
struct Undo(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl Transition<(), ()> for Undo {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: (), _: &(), _: &mut Bus) -> Outcome<(), String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::Next(())
    }
}
#[derive(Clone)]
struct Source {
    finite: bool,
}
#[async_trait::async_trait]
impl StreamingTransition<()> for Source {
    type Item = u32;
    type Error = String;
    type Resources = ();
    async fn run_stream(
        &self,
        _: (),
        _: &(),
        _: &mut Bus,
    ) -> Result<Pin<Box<dyn futures_core::Stream<Item = u32> + Send>>, String> {
        Ok(if self.finite {
            Box::pin(futures_util::stream::iter([1, 2]))
        } else {
            Box::pin(futures_util::StreamExt::chain(
                futures_util::stream::iter([1]),
                futures_util::stream::pending(),
            ))
        })
    }
}

#[tokio::test]
#[ignore = "requires RANVIER_PERSISTENCE_POSTGRES_URL"]
async fn supervised_stream_end_is_recorded_and_released_on_postgres() {
    let url = std::env::var("RANVIER_PERSISTENCE_POSTGRES_URL")
        .expect("RANVIER_PERSISTENCE_POSTGRES_URL");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .acquire_timeout(Duration::from_secs(3))
        .connect(&url)
        .await
        .unwrap();
    let prefix = format!("sse_{}", uuid::Uuid::new_v4().simple());
    let store = PostgresPersistenceStore::with_table_prefix(pool.clone(), &prefix);
    store.ensure_schema().await.unwrap();
    let mut summary = Vec::new();
    for finite in [true, false] {
        let trace = format!(
            "supervised-{}-{}",
            if finite { "completed" } else { "abandoned" },
            uuid::Uuid::new_v4().simple()
        );
        let effects = Arc::new(AtomicUsize::new(0));
        let undos = Arc::new(AtomicUsize::new(0));
        let circuit = Axon::<(), (), String>::new("SupervisedStreamLive")
            .then_compensated(Effect(effects.clone()), Undo(undos.clone()))
            .with_saga_policy(ranvier_core::saga::SagaPolicy::Enabled)
            .then_stream(Source { finite });
        let mut bus = Bus::new();
        bus.insert(PersistenceHandle::from_store(store.clone()));
        bus.insert(PersistenceTraceId::new(trace.clone()));
        let supervisor = StreamSupervisor::new(1);
        let (ticket, context) = supervisor
            .reserve()
            .unwrap()
            .with_context(|| OwnedStreamContext::try_new(Arc::new(()), bus))
            .unwrap();
        let prepared = ticket
            .prepare(&circuit, context, (), Duration::from_secs(5), None)
            .unwrap_or_else(|_| panic!("setup rejected"));
        let mut invocation = prepared
            .start(CancellationToken::new())
            .await
            .unwrap_or_else(|error| panic!("{}", error.signal));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        tokio::time::timeout_at(deadline, invocation.readiness.initialized())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            tokio::time::timeout_at(deadline, invocation.items.recv())
                .await
                .unwrap(),
            Some(1)
        );
        let report = invocation.report().clone();
        if finite {
            while tokio::time::timeout_at(deadline, invocation.items.recv())
                .await
                .unwrap()
                .is_some()
            {}
            drop(invocation);
        } else {
            // The consumer goes away while the source is still Pending.
            drop(invocation);
        }
        report.settle_until(deadline).await.unwrap();
        let end = report.stream_end_durability().unwrap();
        assert!(
            matches!(
                end,
                Some(ranvier_runtime::StreamDurability::Recorded { .. })
            ),
            "{end:?}"
        );
        assert_eq!(report.release_settled_until(deadline).await, Ok(()));
        let stored = store.load(&trace).await.unwrap().expect("trace stored");
        let result = stored
            .events
            .iter()
            .find(|event| {
                event.event_type == PersistenceEventType::Result
                    && event.outcome_kind.starts_with("Stream")
            })
            .expect("stream end result")
            .clone();
        assert_eq!(
            stored.events.last().map(|event| event.event_type),
            Some(PersistenceEventType::Terminal)
        );
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        if finite {
            assert_eq!(result.outcome_kind, "StreamCompleted");
            assert_eq!(stored.completion, Some(CompletionState::Success));
            assert_eq!(
                undos.load(Ordering::SeqCst),
                0,
                "success runs no compensation"
            );
        } else {
            assert_eq!(result.outcome_kind, "StreamAbandoned");
            assert_eq!(stored.completion, Some(CompletionState::Cancelled));
            assert_eq!(
                undos.load(Ordering::SeqCst),
                1,
                "disconnect runs the prefix compensation"
            );
        }
        drop(report);
        assert!(supervisor.reserve().is_ok(), "custody released");
        summary.push(format!(
            "{}={}/{:?}/undo{}",
            if finite { "completed" } else { "abandoned" },
            result.outcome_kind,
            stored.completion,
            undos.load(Ordering::SeqCst)
        ));
        if !finite {
            summary.push(format!(
                "payload={}",
                result.payload.clone().unwrap_or_default()
            ));
        }
    }
    println!(
        "SUPERVISED_STREAM_POSTGRES {} released PASS",
        summary.join(" ")
    );
}
