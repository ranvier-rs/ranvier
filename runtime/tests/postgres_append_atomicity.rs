#![cfg(feature = "persistence-postgres")]
use ranvier_runtime::persistence::{
    CompletionState, Intervention, PersistenceEnvelope, PersistenceStore, PostgresPersistenceStore,
};

fn event(trace: &str, step: u64, version: &str) -> PersistenceEnvelope {
    let (event_type, node_id, outcome_kind) = if step == 0 {
        (
            ranvier_runtime::persistence::PersistenceEventType::Enter,
            None,
            "Enter",
        )
    } else {
        (
            ranvier_runtime::persistence::PersistenceEventType::Checkpoint,
            Some(format!("checkpoint-{step}")),
            "Checkpoint",
        )
    };
    PersistenceEnvelope {
        trace_id: trace.into(),
        circuit: "AtomicFixture".into(),
        schematic_version: version.into(),
        event_sequence: step,
        event_type,
        step,
        node_id,
        outcome_kind: outcome_kind.into(),
        timestamp_ms: 1000,
        payload_hash: None,
        payload: None,
    }
}

fn terminal_event(
    trace: &str,
    sequence: u64,
    step: u64,
    version: &str,
    outcome: &str,
) -> PersistenceEnvelope {
    PersistenceEnvelope {
        trace_id: trace.into(),
        circuit: "AtomicFixture".into(),
        schematic_version: version.into(),
        event_sequence: sequence,
        event_type: ranvier_runtime::persistence::PersistenceEventType::Terminal,
        step,
        node_id: None,
        outcome_kind: outcome.into(),
        timestamp_ms: 1000 + sequence,
        payload_hash: None,
        payload: Some(serde_json::json!({ "terminal": outcome })),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PostgresAckLossTarget {
    CompensationMarkerInsert,
    CommittedTransaction(u8),
}

#[derive(Debug, PartialEq, Eq)]
struct PostgresAckLossObservation {
    target: PostgresAckLossTarget,
    command_tag: &'static str,
    transaction_status: u8,
}

async fn postgres_proxy_drop_ready_for_query_after(
    client: tokio::net::TcpStream,
    database: tokio::net::TcpStream,
    target: PostgresAckLossTarget,
) -> anyhow::Result<PostgresAckLossObservation> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    anyhow::ensure!(
        !matches!(target, PostgresAckLossTarget::CommittedTransaction(0)),
        "committed transaction count is one-based"
    );
    let (mut client_reader, mut client_writer) = client.into_split();
    let (mut database_reader, mut database_writer) = database.into_split();

    let client_to_database = tokio::io::copy(&mut client_reader, &mut database_writer);
    let database_to_client = async {
        let mut saw_compensation_marker_insert = false;
        let mut committed_transaction_count = 0_u8;
        loop {
            let mut header = [0_u8; 5];
            database_reader.read_exact(&mut header).await?;
            let message_length =
                u32::from_be_bytes(header[1..5].try_into().expect("fixed header")) as usize;
            anyhow::ensure!(
                (4..=16 * 1024 * 1024).contains(&message_length),
                "PostgreSQL backend frame length is outside the fixture bound"
            );
            let mut payload = vec![0; message_length - 4];
            database_reader.read_exact(&mut payload).await?;

            if header[0] == b'C' {
                match target {
                    PostgresAckLossTarget::CompensationMarkerInsert
                        if payload.as_slice() == b"INSERT 0 1\0" =>
                    {
                        saw_compensation_marker_insert = true;
                    }
                    PostgresAckLossTarget::CommittedTransaction(_)
                        if payload.as_slice() == b"COMMIT\0" =>
                    {
                        committed_transaction_count = committed_transaction_count.saturating_add(1);
                    }
                    _ => {}
                }
            }
            if header[0] == b'Z' {
                let transaction_status = payload.first().copied();
                let target_seen = match target {
                    PostgresAckLossTarget::CompensationMarkerInsert => {
                        saw_compensation_marker_insert
                    }
                    PostgresAckLossTarget::CommittedTransaction(count) => {
                        committed_transaction_count == count
                    }
                };
                if target_seen && transaction_status == Some(b'I') {
                    // PostgreSQL reports an idle session after the targeted
                    // compensation marker insert or transaction commit. Drop
                    // that response so the caller sees an unknown ACK.
                    let command_tag = match target {
                        PostgresAckLossTarget::CompensationMarkerInsert => "INSERT 0 1",
                        PostgresAckLossTarget::CommittedTransaction(_) => "COMMIT",
                    };
                    return Ok(PostgresAckLossObservation {
                        target,
                        command_tag,
                        transaction_status: b'I',
                    });
                }
                saw_compensation_marker_insert = false;
            }

            client_writer.write_all(&header).await?;
            client_writer.write_all(&payload).await?;
        }
    };

    tokio::select! {
        result = client_to_database => {
            Err(anyhow::anyhow!("client connection ended before the targeted ReadyForQuery was dropped: {result:?}"))
        }
        result = database_to_client => result,
    }
}

#[tokio::test]
#[ignore = "requires explicitly isolated PostgreSQL fixture URL"]
async fn postgres_legacy_null_blocks_execution_and_pre_cancelled_cleanup() {
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        use ranvier_core::cancellation::{CancellationReason, CancellationToken};
        use ranvier_core::prelude::*;
        use ranvier_runtime::persistence::{
            CompensationContext, CompensationHandle, CompensationHook, PersistenceHandle,
            PersistenceTraceId,
        };
        use ranvier_runtime::{Axon, EffectBoundary, ExecutionReport, PersistenceOperation};
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        #[derive(Clone)]
        struct Count(Arc<AtomicUsize>);
        #[async_trait::async_trait]
        impl Transition<u32, u32> for Count {
            type Error = String;
            type Resources = ();
            async fn run(&self, input: u32, _: &(), _: &mut Bus) -> Outcome<u32, String> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Outcome::Next(input + 1)
            }
        }
        #[async_trait::async_trait]
        impl CompensationHook for Count {
            async fn compensate(&self, _: CompensationContext) -> anyhow::Result<()> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }

        let url = std::env::var("RANVIER_ATOMICITY_POSTGRES_URL")
            .expect("explicit isolated database required");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .acquire_timeout(std::time::Duration::from_secs(3))
            .connect(&url)
            .await
            .unwrap();
        let prefix = format!("null_{}", uuid::Uuid::new_v4().simple());
        let store = PostgresPersistenceStore::with_table_prefix(pool.clone(), &prefix);
        store.ensure_schema().await.unwrap();
        for mode in [0, 1, 2] {
            let trace_id = format!("null-mode-{mode}");
            let effects = Arc::new(AtomicUsize::new(0));
            let cleanup = Arc::new(AtomicUsize::new(0));
            let axon = Axon::<u32, u32, String>::new("AtomicFixture").then(Count(effects.clone()));
            let schematic = axon.schematic();
            store
                .append(event(&trace_id, 0, &schematic.schema_version))
                .await
                .unwrap();
            store
                .append(PersistenceEnvelope {
                    trace_id: trace_id.clone(),
                    circuit: schematic.name.clone(),
                    schematic_version: schematic.schema_version.clone(),
                    event_sequence: 1,
                    event_type: ranvier_runtime::persistence::PersistenceEventType::Intent,
                    step: 1,
                    node_id: Some(schematic.nodes[1].id.clone()),
                    outcome_kind: "Intent".into(),
                    timestamp_ms: 1000,
                    payload_hash: None,
                    payload: Some(serde_json::json!(7)),
                })
                .await
                .unwrap();
            store
                .append(PersistenceEnvelope {
                    trace_id: trace_id.clone(),
                    circuit: schematic.name.clone(),
                    schematic_version: schematic.schema_version.clone(),
                    event_sequence: 2,
                    event_type: ranvier_runtime::persistence::PersistenceEventType::Result,
                    step: 1,
                    node_id: Some(schematic.nodes[1].id.clone()),
                    outcome_kind: "Next".into(),
                    timestamp_ms: 1001,
                    payload_hash: None,
                    payload: Some(serde_json::Value::Null),
                })
                .await
                .unwrap();
            let before = store.load(&trace_id).await.unwrap().unwrap();
            let mut bus = Bus::new();
            bus.insert(PersistenceTraceId::new(trace_id.clone()));
            bus.insert(PersistenceHandle::from_store(store.clone()));
            bus.insert(CompensationHandle::from_hook(Count(cleanup.clone())));
            if mode == 0 {
                assert!(matches!(
                    axon.execute(7, &(), &mut bus).await,
                    ExecutionReport::BlockedBeforeEffect {
                        operation: PersistenceOperation::RecoveryPayload,
                        effect: EffectBoundary::NotInvoked,
                        ..
                    }
                ));
            } else {
                let token = CancellationToken::new();
                if mode == 2 {
                    let _ = token.cancel(CancellationReason::Explicit);
                }
                assert!(matches!(
                    axon.execute_cancellable(7, &(), &mut bus, token).await,
                    ExecutionReport::BlockedBeforeEffect {
                        operation: PersistenceOperation::RecoveryPayload,
                        effect: EffectBoundary::NotInvoked,
                        ..
                    }
                ));
            }
            assert_eq!(effects.load(Ordering::SeqCst), 0);
            assert_eq!(cleanup.load(Ordering::SeqCst), 0);
            assert_eq!(store.load(&trace_id).await.unwrap().unwrap(), before);
        }
        pool.close().await;
        println!("POSTGRES_LEGACY_NULL modes=3 effects=0 cleanup=0 full-traces-unchanged");
    })
    .await
    .expect("isolated recovery fixture must finish within 15 seconds");
}

// The monotonic event sequence must permit a final terminal event without
// colliding with the last transition result.
#[tokio::test]
#[ignore = "requires explicitly isolated PostgreSQL fixture URL"]
async fn postgres_executor_event_sequence_prevents_terminal_collision() {
    use ranvier_core::prelude::*;
    use ranvier_runtime::persistence::{PersistenceHandle, PersistenceTraceId};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[derive(Clone)]
    struct CountStep(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl Transition<u32, u32> for CountStep {
        type Error = String;
        type Resources = ();
        async fn run(&self, input: u32, _: &(), _: &mut Bus) -> Outcome<u32, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Outcome::next(input + 1)
        }
    }
    let url = std::env::var("RANVIER_ATOMICITY_POSTGRES_URL")
        .expect("explicit isolated database required");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(std::time::Duration::from_secs(3))
        .connect(&url)
        .await
        .unwrap();
    let prefix = format!("terminal_{}", uuid::Uuid::new_v4().simple());
    let store = PostgresPersistenceStore::with_table_prefix(pool.clone(), &prefix);
    store.ensure_schema().await.unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let axon = ranvier_runtime::Axon::<u32, u32, String, ()>::new("TerminalCollisionFixture")
        .then(CountStep(calls.clone()))
        .then(CountStep(calls.clone()));
    let mut bus = Bus::new();
    bus.insert(PersistenceHandle::from_store(store.clone()));
    bus.insert(PersistenceTraceId::new("terminal-collision"));
    assert!(matches!(
        axon.execute(0, &(), &mut bus).await,
        ranvier_runtime::ExecutionReport::Recorded {
            result: Outcome::Next(2),
            completion: ranvier_runtime::CompletionConfirmation::Confirmed,
            ..
        }
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let trace = store.load("terminal-collision").await.unwrap().unwrap();
    assert_eq!(trace.completion, Some(CompletionState::Success));
    assert_eq!(
        trace.events.iter().map(|e| e.step).collect::<Vec<_>>(),
        vec![0, 1, 1, 2, 2, 3]
    );
    assert_eq!(trace.events[0].outcome_kind, "Enter");
    let first = Outcome::<u32, String>::next(1).to_json_value();
    let terminal = Outcome::<u32, String>::next(2).to_json_value();
    assert_eq!(
        trace.events[1].event_type,
        ranvier_runtime::PersistenceEventType::Intent
    );
    assert_eq!(
        trace.events[2].event_type,
        ranvier_runtime::PersistenceEventType::Result
    );
    assert_eq!(trace.events[2].payload, Some(first));
    assert_eq!(
        trace.events[3].event_type,
        ranvier_runtime::PersistenceEventType::Intent
    );
    assert_eq!(
        trace.events[4].event_type,
        ranvier_runtime::PersistenceEventType::Result
    );
    assert_eq!(trace.events[4].payload, Some(terminal.clone()));
    assert_eq!(
        trace.events[5].event_type,
        ranvier_runtime::PersistenceEventType::Terminal
    );
    assert_eq!(trace.events[5].payload, Some(terminal));
    let reentry = axon.execute(0, &(), &mut bus).await;
    assert!(matches!(
        reentry,
        ranvier_runtime::ExecutionReport::NotRequested {
            result: Outcome::Emit(signal, None),
            ..
        } if signal == "execution.resumption.completed_trace"
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        store.load("terminal-collision").await.unwrap().unwrap(),
        trace
    );
    println!(
        "POSTGRES_TERMINAL_EVENT_SEQUENCE reentry-completed-trace-blocked effects=2 completion-success full-trace-unchanged"
    );
    pool.close().await;
}

// Simulates loss of the acknowledgement after the actual PostgreSQL store has
// returned success. This characterizes the PersistenceStore caller boundary;
// it is not a SQLx/network proxy test and does not establish recovery policy.
#[tokio::test]
#[ignore = "requires explicitly isolated PostgreSQL fixture URL"]
async fn postgres_committed_result_with_lost_store_ack_is_reported_unconfirmed() {
    use ranvier_core::prelude::*;
    use ranvier_runtime::persistence::{
        CompletionState, Intervention, PersistenceHandle, PersistenceTraceId,
        PostgresPersistenceStore, ResumeCursor,
    };
    use ranvier_runtime::{EffectBoundary, ExecutionReport, PersistenceOperation};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    #[derive(Clone)]
    struct CountEffect(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl Transition<u32, u32> for CountEffect {
        type Error = String;
        type Resources = ();
        async fn run(&self, input: u32, _: &(), _: &mut Bus) -> Outcome<u32, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Outcome::next(input + 1)
        }
    }

    #[derive(Clone)]
    struct CommitThenLoseAck {
        inner: PostgresPersistenceStore,
        lost: Arc<AtomicBool>,
    }
    #[async_trait::async_trait]
    impl PersistenceStore for CommitThenLoseAck {
        async fn append(&self, envelope: PersistenceEnvelope) -> anyhow::Result<()> {
            let is_result = envelope.outcome_kind == "Next" && envelope.step == 1;
            let inject_loss = is_result && !self.lost.load(Ordering::SeqCst);
            self.inner.append(envelope).await?;
            if inject_loss && !self.lost.swap(true, Ordering::SeqCst) {
                return Err(anyhow::anyhow!("private-committed-ack-loss-sentinel"));
            }
            Ok(())
        }

        async fn load(
            &self,
            trace_id: &str,
        ) -> anyhow::Result<Option<ranvier_runtime::persistence::PersistedTrace>> {
            self.inner.load(trace_id).await
        }

        async fn resume(&self, trace_id: &str, step: u64) -> anyhow::Result<ResumeCursor> {
            self.inner.resume(trace_id, step).await
        }

        async fn complete(
            &self,
            trace_id: &str,
            completion: CompletionState,
        ) -> anyhow::Result<()> {
            self.inner.complete(trace_id, completion).await
        }

        async fn save_intervention(
            &self,
            trace_id: &str,
            intervention: Intervention,
        ) -> anyhow::Result<()> {
            self.inner.save_intervention(trace_id, intervention).await
        }
    }

    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        let url = std::env::var("RANVIER_ATOMICITY_POSTGRES_URL")
            .expect("explicit isolated database required");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .acquire_timeout(std::time::Duration::from_secs(3))
            .connect(&url)
            .await
            .unwrap();
        let prefix = format!("ack_loss_{}", uuid::Uuid::new_v4().simple());
        let store = PostgresPersistenceStore::with_table_prefix(pool.clone(), &prefix);
        store.ensure_schema().await.unwrap();

        let lost = Arc::new(AtomicBool::new(false));
        let effects = Arc::new(AtomicUsize::new(0));
        let axon = ranvier_runtime::Axon::<u32, u32, String, ()>::new("PostgresAckLossFixture")
            .then(CountEffect(effects.clone()));
        let mut bus = Bus::new();
        bus.insert(PersistenceHandle::from_store(CommitThenLoseAck {
            inner: store.clone(),
            lost: lost.clone(),
        }));
        bus.insert(PersistenceTraceId::new("committed-result-ack-loss"));

        let report = axon.execute(0, &(), &mut bus).await;
        assert!(matches!(
            &report,
            ExecutionReport::Unconfirmed {
                result: Outcome::Next(1),
                operation: PersistenceOperation::Result,
                effect: EffectBoundary::MayHaveRun,
                ..
            }
        ), "unexpected committed-result report: {report:?}; effects={}", effects.load(Ordering::SeqCst));
        assert!(!format!("{report:?}").contains("private-committed-ack-loss-sentinel"));
        assert!(lost.load(Ordering::SeqCst));
        assert_eq!(effects.load(Ordering::SeqCst), 1);

        // Read through a fresh store handle to observe what PostgreSQL actually
        // committed even though the caller received an error for that append.
        let fresh_reader = PostgresPersistenceStore::with_table_prefix(pool.clone(), &prefix);
        let trace = fresh_reader
            .load("committed-result-ack-loss")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(trace.events.len(), 3);
        assert_eq!(trace.events[0].outcome_kind, "Enter");
        assert_eq!(
            trace.events[1].event_type,
            ranvier_runtime::persistence::PersistenceEventType::Intent
        );
        assert_eq!(trace.events[1].step, 1);
        assert_eq!(trace.events[2].event_type, ranvier_runtime::persistence::PersistenceEventType::Result);
        assert_eq!(trace.events[2].outcome_kind, "Next");
        assert_eq!(trace.events[2].step, 1);
        assert_eq!(
            trace.events[2].payload,
            Some(Outcome::<u32, String>::next(1).to_json_value())
        );
        assert_eq!(trace.completion, None);
        pool.close().await;
        println!(
            "POSTGRES_COMMITTED_RESULT_ACK_LOST result=Next(1) effect=1 stored-result-present completion=unset"
        );
    })
    .await
    .expect("isolated committed-write acknowledgement fixture must finish within 15 seconds");
}

// Exercises the complete acknowledgement boundary after PostgreSQL has
// durably stored the completion state. This remains a caller/store wrapper,
// not a SQLx/network interruption or recovery-policy test.
#[tokio::test]
#[ignore = "requires explicitly isolated PostgreSQL fixture URL"]
async fn postgres_committed_completion_with_lost_store_ack_is_reported_unconfirmed() {
    use ranvier_core::prelude::*;
    use ranvier_runtime::persistence::{
        CompletionState, Intervention, PersistenceHandle, PersistenceTraceId,
        PostgresPersistenceStore, ResumeCursor,
    };
    use ranvier_runtime::{EffectBoundary, ExecutionReport, PersistenceOperation};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    #[derive(Clone)]
    struct CountEffect(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl Transition<u32, u32> for CountEffect {
        type Error = String;
        type Resources = ();
        async fn run(&self, input: u32, _: &(), _: &mut Bus) -> Outcome<u32, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Outcome::next(input + 1)
        }
    }

    #[derive(Clone)]
    struct CommitThenLoseCompleteAck {
        inner: PostgresPersistenceStore,
        lost: Arc<AtomicBool>,
        calls: Arc<AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl PersistenceStore for CommitThenLoseCompleteAck {
        async fn append(&self, envelope: PersistenceEnvelope) -> anyhow::Result<()> {
            self.inner.append(envelope).await
        }

        async fn load(
            &self,
            trace_id: &str,
        ) -> anyhow::Result<Option<ranvier_runtime::persistence::PersistedTrace>> {
            self.inner.load(trace_id).await
        }

        async fn resume(&self, trace_id: &str, step: u64) -> anyhow::Result<ResumeCursor> {
            self.inner.resume(trace_id, step).await
        }

        async fn complete(
            &self,
            trace_id: &str,
            completion: CompletionState,
        ) -> anyhow::Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.inner.complete(trace_id, completion).await?;
            if !self.lost.swap(true, Ordering::SeqCst) {
                return Err(anyhow::anyhow!(
                    "private-committed-completion-ack-loss-sentinel"
                ));
            }
            Ok(())
        }

        async fn save_intervention(
            &self,
            trace_id: &str,
            intervention: Intervention,
        ) -> anyhow::Result<()> {
            self.inner.save_intervention(trace_id, intervention).await
        }
    }

    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        let url = std::env::var("RANVIER_ATOMICITY_POSTGRES_URL")
            .expect("explicit isolated database required");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .acquire_timeout(std::time::Duration::from_secs(3))
            .connect(&url)
            .await
            .unwrap();
        let prefix = format!("complete_ack_{}", uuid::Uuid::new_v4().simple());
        let store = PostgresPersistenceStore::with_table_prefix(pool.clone(), &prefix);
        store.ensure_schema().await.unwrap();

        let lost = Arc::new(AtomicBool::new(false));
        let complete_calls = Arc::new(AtomicUsize::new(0));
        let effects = Arc::new(AtomicUsize::new(0));
        let axon = ranvier_runtime::Axon::<u32, u32, String, ()>::new(
            "PostgresCompleteAckLossFixture",
        )
        .then(CountEffect(effects.clone()));
        let mut bus = Bus::new();
        bus.insert(PersistenceHandle::from_store(CommitThenLoseCompleteAck {
            inner: store.clone(),
            lost: lost.clone(),
            calls: complete_calls.clone(),
        }));
        bus.insert(PersistenceTraceId::new("committed-completion-ack-loss"));

        let report = axon.execute(7, &(), &mut bus).await;
        assert!(matches!(
            &report,
            ExecutionReport::Unconfirmed {
                result: Outcome::Next(8),
                operation: PersistenceOperation::Complete,
                effect: EffectBoundary::MayHaveRun,
                ..
            }
        ));
        assert!(!format!("{report:?}")
            .contains("private-committed-completion-ack-loss-sentinel"));
        assert!(lost.load(Ordering::SeqCst));
        assert_eq!(complete_calls.load(Ordering::SeqCst), 1);
        assert_eq!(effects.load(Ordering::SeqCst), 1);

        let reader_pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(3))
            .connect(&url)
            .await
            .unwrap();
        let fresh_reader = PostgresPersistenceStore::with_table_prefix(reader_pool.clone(), &prefix);
        let trace = fresh_reader
            .load("committed-completion-ack-loss")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(trace.events.len(), 4);
        assert_eq!(trace.events[0].outcome_kind, "Enter");
        assert_eq!(trace.events[0].step, 0);
        assert_eq!(
            trace.events[1].event_type,
            ranvier_runtime::persistence::PersistenceEventType::Intent
        );
        assert_eq!(trace.events[2].event_type, ranvier_runtime::persistence::PersistenceEventType::Result);
        assert_eq!(trace.events[2].outcome_kind, "Next");
        assert_eq!(trace.events[2].step, 1);
        assert_eq!(
            trace.events[2].payload,
            Some(Outcome::<u32, String>::next(8).to_json_value())
        );
        assert_eq!(
            trace.events[3].event_type,
            ranvier_runtime::persistence::PersistenceEventType::Terminal
        );
        assert_eq!(trace.completion, Some(CompletionState::Success));
        reader_pool.close().await;
        pool.close().await;
        println!(
            "POSTGRES_COMMITTED_COMPLETION_ACK_LOST result=Next(8) effects=1 complete-calls=1 terminal-row-present completion=Success"
        );
    })
    .await
    .expect("isolated committed-completion acknowledgement fixture must finish within 15 seconds");
}

// Drops the sixth ReadyForQuery after COMMIT: load, Enter, Intent, Result,
// Terminal, then completion. PostgreSQL has applied the completion before the
// caller loses the transaction acknowledgement.
#[tokio::test]
#[ignore = "requires explicitly isolated PostgreSQL fixture URL"]
async fn postgres_completion_ack_lost_on_tcp_after_commit_is_unconfirmed() {
    use ranvier_core::prelude::*;
    use ranvier_runtime::persistence::{
        CompletionState, PersistenceHandle, PersistenceTraceId, PostgresPersistenceStore,
    };
    use ranvier_runtime::{EffectBoundary, ExecutionReport, PersistenceOperation};
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::net::{TcpListener, TcpStream};

    #[derive(Clone)]
    struct CountEffect(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl Transition<u32, u32> for CountEffect {
        type Error = String;
        type Resources = ();
        async fn run(&self, input: u32, _: &(), _: &mut Bus) -> Outcome<u32, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Outcome::next(input + 1)
        }
    }

    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        let url = std::env::var("RANVIER_ATOMICITY_POSTGRES_URL")
            .expect("explicit isolated database required");
        let database_options = url.parse::<PgConnectOptions>().unwrap();
        let upstream_host = database_options.get_host().to_owned();
        let upstream_port = database_options.get_port();
        let direct_pool = PgPoolOptions::new()
            .max_connections(2)
            .acquire_timeout(std::time::Duration::from_secs(3))
            .connect(&url)
            .await
            .unwrap();
        let prefix = format!("wire_ack_{}", uuid::Uuid::new_v4().simple());
        let direct_store = PostgresPersistenceStore::with_table_prefix(direct_pool.clone(), &prefix);
        direct_store.ensure_schema().await.unwrap();

        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let proxy_address = listener.local_addr().unwrap();
        let proxy_task = tokio::spawn(async move {
            let (client, _) = listener.accept().await?;
            let database = TcpStream::connect((upstream_host.as_str(), upstream_port)).await?;
            postgres_proxy_drop_ready_for_query_after(
                client,
                database,
                PostgresAckLossTarget::CommittedTransaction(6),
            )
            .await
        });
        let proxy_options = database_options
            .host("127.0.0.1")
            .port(proxy_address.port())
            .ssl_mode(PgSslMode::Disable);
        let proxy_pool = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(3))
            .connect_with(proxy_options)
            .await
            .unwrap();
        let proxy_store = PostgresPersistenceStore::with_table_prefix(proxy_pool.clone(), &prefix);
        let effects = Arc::new(AtomicUsize::new(0));
        let axon = ranvier_runtime::Axon::<u32, u32, String, ()>::new(
            "PostgresWireCompletionAckLossFixture",
        )
        .then(CountEffect(effects.clone()));
        let mut bus = Bus::new();
        bus.insert(PersistenceHandle::from_store(proxy_store));
        bus.insert(PersistenceTraceId::new("wire-completion-ack-loss"));

        let report = axon.execute(7, &(), &mut bus).await;
        assert!(matches!(
            &report,
            ExecutionReport::Unconfirmed {
                result: Outcome::Next(8),
                operation: PersistenceOperation::Complete,
                effect: EffectBoundary::MayHaveRun,
                ..
            }
        ), "unexpected TCP completion-ack report: {report:?}; effects={}", effects.load(Ordering::SeqCst));
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        let proxy_task_observation = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            proxy_task,
        )
        .await
        .expect("TCP proxy must observe the completion response")
        .expect("TCP proxy task must not panic")
        .expect("TCP proxy must relay and drop the intended backend frame");
        assert_eq!(
            proxy_task_observation,
            PostgresAckLossObservation {
                target: PostgresAckLossTarget::CommittedTransaction(6),
                command_tag: "COMMIT",
                transaction_status: b'I',
            }
        );
        proxy_pool.close().await;

        let reader_pool = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(3))
            .connect(&url)
            .await
            .unwrap();
        let reader = PostgresPersistenceStore::with_table_prefix(reader_pool.clone(), &prefix);
        let trace = reader
            .load("wire-completion-ack-loss")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(trace.events.len(), 4);
        assert_eq!(trace.events[0].outcome_kind, "Enter");
        assert_eq!(trace.events[0].step, 0);
        assert_eq!(
            trace.events[1].event_type,
            ranvier_runtime::persistence::PersistenceEventType::Intent
        );
        assert_eq!(trace.events[2].event_type, ranvier_runtime::persistence::PersistenceEventType::Result);
        assert_eq!(trace.events[2].outcome_kind, "Next");
        assert_eq!(trace.events[2].step, 1);
        assert_eq!(
            trace.events[2].payload,
            Some(Outcome::<u32, String>::next(8).to_json_value())
        );
        assert_eq!(
            trace.events[3].event_type,
            ranvier_runtime::persistence::PersistenceEventType::Terminal
        );
        assert_eq!(trace.completion, Some(CompletionState::Success));
        reader_pool.close().await;
        direct_pool.close().await;
        println!(
            "POSTGRES_TCP_COMPLETION_ACK_LOST dropped=ReadyForQuery-after-COMMIT-6 caller=Unconfirmed stored=Success"
        );
    })
    .await
    .expect("isolated TCP completion acknowledgement fixture must finish within 15 seconds");
}

// Drops the initial PostgreSQL load transaction's ReadyForQuery after commit.
// The persisted trace must remain untouched and execution must block before
// invoking its first transition because the caller cannot trust the cursor.
#[tokio::test]
#[ignore = "requires explicitly isolated PostgreSQL fixture URL"]
async fn postgres_load_ack_lost_on_tcp_blocks_before_effect() {
    use ranvier_core::prelude::*;
    use ranvier_runtime::persistence::{
        PersistenceHandle, PersistenceTraceId, PostgresPersistenceStore,
    };
    use ranvier_runtime::{EffectBoundary, ExecutionReport, PersistenceOperation};
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::net::{TcpListener, TcpStream};

    #[derive(Clone)]
    struct CountEffect(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl Transition<u32, u32> for CountEffect {
        type Error = String;
        type Resources = ();
        async fn run(&self, input: u32, _: &(), _: &mut Bus) -> Outcome<u32, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Outcome::next(input + 1)
        }
    }

    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        let url = std::env::var("RANVIER_ATOMICITY_POSTGRES_URL")
            .expect("explicit isolated database required");
        let database_options = url.parse::<PgConnectOptions>().unwrap();
        let upstream_host = database_options.get_host().to_owned();
        let upstream_port = database_options.get_port();
        let direct_pool = PgPoolOptions::new()
            .max_connections(2)
            .acquire_timeout(std::time::Duration::from_secs(3))
            .connect(&url)
            .await
            .unwrap();
        let prefix = format!("wire_load_ack_{}", uuid::Uuid::new_v4().simple());
        let direct_store = PostgresPersistenceStore::with_table_prefix(direct_pool.clone(), &prefix);
        direct_store.ensure_schema().await.unwrap();

        let effects = Arc::new(AtomicUsize::new(0));
        let axon = ranvier_runtime::Axon::<u32, u32, String, ()>::new(
            "PostgresWireLoadAckLossFixture",
        )
        .then(CountEffect(effects.clone()));
        let trace_id = "wire-load-ack-loss";
        let mut enter = event(trace_id, 0, &axon.schematic().schema_version);
        enter.circuit = "PostgresWireLoadAckLossFixture".into();
        enter.outcome_kind = "Enter".into();
        direct_store.append(enter).await.unwrap();

        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let proxy_address = listener.local_addr().unwrap();
        let proxy_task = tokio::spawn(async move {
            let (client, _) = listener.accept().await?;
            let database = TcpStream::connect((upstream_host.as_str(), upstream_port)).await?;
            postgres_proxy_drop_ready_for_query_after(
                client,
                database,
                PostgresAckLossTarget::CommittedTransaction(1),
            )
            .await
        });
        let proxy_options = database_options
            .host("127.0.0.1")
            .port(proxy_address.port())
            .ssl_mode(PgSslMode::Disable);
        let proxy_pool = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(3))
            .connect_with(proxy_options)
            .await
            .unwrap();
        let proxy_store = PostgresPersistenceStore::with_table_prefix(proxy_pool.clone(), &prefix);
        let mut bus = Bus::new();
        bus.insert(PersistenceHandle::from_store(proxy_store));
        bus.insert(PersistenceTraceId::new(trace_id));

        let report = axon.execute(0, &(), &mut bus).await;
        assert!(matches!(
            &report,
            ExecutionReport::BlockedBeforeEffect {
                operation: PersistenceOperation::Load,
                effect: EffectBoundary::NotInvoked,
                ..
            }
        ), "unexpected load-ack report: {report:?}; effects={}", effects.load(Ordering::SeqCst));
        assert_eq!(effects.load(Ordering::SeqCst), 0);
        let proxy_task_observation = tokio::time::timeout(std::time::Duration::from_secs(3), proxy_task)
            .await
            .expect("TCP proxy must observe the initial load commit response")
            .expect("TCP proxy task must not panic")
            .expect("TCP proxy must relay and drop the targeted backend frame");
        assert_eq!(
            proxy_task_observation,
            PostgresAckLossObservation {
                target: PostgresAckLossTarget::CommittedTransaction(1),
                command_tag: "COMMIT",
                transaction_status: b'I',
            }
        );
        proxy_pool.close().await;

        let reader_pool = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(3))
            .connect(&url)
            .await
            .unwrap();
        let reader = PostgresPersistenceStore::with_table_prefix(reader_pool.clone(), &prefix);
        let trace = reader.load(trace_id).await.unwrap().unwrap();
        assert_eq!(trace.events.len(), 1);
        assert_eq!(trace.events[0].outcome_kind, "Enter");
        assert_eq!(trace.events[0].step, 0);
        assert_eq!(trace.completion, None);
        reader_pool.close().await;
        direct_pool.close().await;
        println!(
            "POSTGRES_TCP_LOAD_ACK_LOST dropped=ReadyForQuery-after-COMMIT-1 operation=Load effect=NotInvoked effects=0 stored-events=Enter-only completion=unset"
        );
    })
    .await
    .expect("isolated TCP load acknowledgement fixture must finish within 15 seconds");
}

// Drops the Enter append transaction's ReadyForQuery after COMMIT #2. The
// initial load is COMMIT #1; the uncertain intent must block before effects.
#[tokio::test]
#[ignore = "requires explicitly isolated PostgreSQL fixture URL"]
async fn postgres_intent_ack_lost_on_tcp_blocks_before_effect() {
    use ranvier_core::prelude::*;
    use ranvier_runtime::persistence::{
        PersistenceHandle, PersistenceTraceId, PostgresPersistenceStore,
    };
    use ranvier_runtime::{EffectBoundary, ExecutionReport, PersistenceOperation};
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::net::{TcpListener, TcpStream};

    #[derive(Clone)]
    struct CountEffect(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl Transition<u32, u32> for CountEffect {
        type Error = String;
        type Resources = ();
        async fn run(&self, input: u32, _: &(), _: &mut Bus) -> Outcome<u32, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Outcome::next(input + 1)
        }
    }

    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        let url = std::env::var("RANVIER_ATOMICITY_POSTGRES_URL")
            .expect("explicit isolated database required");
        let database_options = url.parse::<PgConnectOptions>().unwrap();
        let upstream_host = database_options.get_host().to_owned();
        let upstream_port = database_options.get_port();
        let direct_pool = PgPoolOptions::new()
            .max_connections(2)
            .acquire_timeout(std::time::Duration::from_secs(3))
            .connect(&url)
            .await
            .unwrap();
        let prefix = format!("intent_ack_{}", uuid::Uuid::new_v4().simple());
        let direct_store = PostgresPersistenceStore::with_table_prefix(direct_pool.clone(), &prefix);
        direct_store.ensure_schema().await.unwrap();

        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let proxy_address = listener.local_addr().unwrap();
        let proxy_task = tokio::spawn(async move {
            let (client, _) = listener.accept().await?;
            let database = TcpStream::connect((upstream_host.as_str(), upstream_port)).await?;
            postgres_proxy_drop_ready_for_query_after(
                client,
                database,
                PostgresAckLossTarget::CommittedTransaction(3),
            )
            .await
        });
        let proxy_options = database_options
            .host("127.0.0.1")
            .port(proxy_address.port())
            .ssl_mode(PgSslMode::Disable);
        let proxy_pool = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(3))
            .connect_with(proxy_options)
            .await
            .unwrap();
        let proxy_store = PostgresPersistenceStore::with_table_prefix(proxy_pool.clone(), &prefix);
        let effects = Arc::new(AtomicUsize::new(0));
        let axon = ranvier_runtime::Axon::<u32, u32, String, ()>::new(
            "PostgresWireIntentAckLossFixture",
        )
        .then(CountEffect(effects.clone()));
        let trace_id = "wire-intent-ack-loss";
        let mut bus = Bus::new();
        bus.insert(PersistenceHandle::from_store(proxy_store));
        bus.insert(PersistenceTraceId::new(trace_id));

        let report = axon.execute(0, &(), &mut bus).await;
        assert!(matches!(
            &report,
            ExecutionReport::BlockedBeforeEffect {
                operation: PersistenceOperation::Intent,
                effect: EffectBoundary::NotInvoked,
                ..
            }
        ), "unexpected intent-ack report: {report:?}; effects={}", effects.load(Ordering::SeqCst));
        assert_eq!(effects.load(Ordering::SeqCst), 0);
        let proxy_task_observation =
            tokio::time::timeout(std::time::Duration::from_secs(3), proxy_task)
                .await
                .expect("TCP proxy must observe the transition Intent append commit response")
                .expect("TCP proxy task must not panic")
                .expect("TCP proxy must relay and drop the targeted backend frame");
        assert_eq!(
            proxy_task_observation,
            PostgresAckLossObservation {
                target: PostgresAckLossTarget::CommittedTransaction(3),
                command_tag: "COMMIT",
                transaction_status: b'I',
            }
        );
        proxy_pool.close().await;

        let reader_pool = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(3))
            .connect(&url)
            .await
            .unwrap();
        let reader = PostgresPersistenceStore::with_table_prefix(reader_pool.clone(), &prefix);
        let trace = reader.load(trace_id).await.unwrap().unwrap();
        assert_eq!(trace.events.len(), 2);
        assert_eq!(trace.events[0].outcome_kind, "Enter");
        assert_eq!(trace.events[0].step, 0);
        assert_eq!(
            trace.events[1].event_type,
            ranvier_runtime::persistence::PersistenceEventType::Intent
        );
        assert_eq!(trace.events[1].step, 1);
        assert_eq!(trace.completion, None);
        reader_pool.close().await;
        direct_pool.close().await;
        println!(
            "POSTGRES_TCP_INTENT_ACK_LOST dropped=ReadyForQuery-after-COMMIT-3 operation=Intent effect=NotInvoked effects=0 stored-events=Enter+Intent completion=unset"
        );
    })
    .await
    .expect("isolated TCP intent acknowledgement fixture must finish within 15 seconds");
}

// Execution commits load, Enter, Intent, then Result. Drop the fourth
// ReadyForQuery after COMMIT to exercise real result-append ACK loss.
#[tokio::test]
#[ignore = "requires explicitly isolated PostgreSQL fixture URL"]
async fn postgres_result_append_ack_lost_on_tcp_after_commit_is_unconfirmed() {
    use ranvier_core::prelude::*;
    use ranvier_runtime::persistence::{
        PersistenceHandle, PersistenceTraceId, PostgresPersistenceStore,
    };
    use ranvier_runtime::{EffectBoundary, ExecutionReport, PersistenceOperation};
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::net::{TcpListener, TcpStream};

    #[derive(Clone)]
    struct CountResultEffect(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl Transition<u32, u32> for CountResultEffect {
        type Error = String;
        type Resources = ();
        async fn run(&self, input: u32, _: &(), _: &mut Bus) -> Outcome<u32, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Outcome::next(input + 1)
        }
    }

    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        let url = std::env::var("RANVIER_ATOMICITY_POSTGRES_URL")
            .expect("explicit isolated database required");
        let database_options = url.parse::<PgConnectOptions>().unwrap();
        let upstream_host = database_options.get_host().to_owned();
        let upstream_port = database_options.get_port();
        let direct_pool = PgPoolOptions::new()
            .max_connections(2)
            .acquire_timeout(std::time::Duration::from_secs(3))
            .connect(&url)
            .await
            .unwrap();
        let prefix = format!("result_ack_{}", uuid::Uuid::new_v4().simple());
        let direct_store = PostgresPersistenceStore::with_table_prefix(direct_pool.clone(), &prefix);
        direct_store.ensure_schema().await.unwrap();

        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let proxy_address = listener.local_addr().unwrap();
        let proxy_task = tokio::spawn(async move {
            let (client, _) = listener.accept().await?;
            let database = TcpStream::connect((upstream_host.as_str(), upstream_port)).await?;
            postgres_proxy_drop_ready_for_query_after(
                client,
                database,
                PostgresAckLossTarget::CommittedTransaction(4),
            )
            .await
        });
        let proxy_options = database_options
            .host("127.0.0.1")
            .port(proxy_address.port())
            .ssl_mode(PgSslMode::Disable);
        let proxy_pool = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(3))
            .connect_with(proxy_options)
            .await
            .unwrap();
        let proxy_store = PostgresPersistenceStore::with_table_prefix(proxy_pool.clone(), &prefix);
        let effects = Arc::new(AtomicUsize::new(0));
        let axon = ranvier_runtime::Axon::<u32, u32, String, ()>::new(
            "PostgresWireResultAckLossFixture",
        )
        .then(CountResultEffect(effects.clone()));
        let mut bus = Bus::new();
        bus.insert(PersistenceHandle::from_store(proxy_store));
        bus.insert(PersistenceTraceId::new("wire-result-ack-loss"));

        let report = axon.execute(0, &(), &mut bus).await;
        assert!(matches!(
            &report,
            ExecutionReport::Unconfirmed {
                result: Outcome::Next(1),
                operation: PersistenceOperation::Result,
                effect: EffectBoundary::MayHaveRun,
                ..
            }
        ), "unexpected result-append report: {report:?}; effects={}", effects.load(Ordering::SeqCst));
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        let proxy_task_observation = tokio::time::timeout(std::time::Duration::from_secs(3), proxy_task)
            .await
            .expect("TCP proxy must observe the result append commit response")
            .expect("TCP proxy task must not panic")
            .expect("TCP proxy must relay and drop the targeted commit response");
        assert_eq!(
            proxy_task_observation,
            PostgresAckLossObservation {
                target: PostgresAckLossTarget::CommittedTransaction(4),
                command_tag: "COMMIT",
                transaction_status: b'I',
            }
        );
        proxy_pool.close().await;

        let reader_pool = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(3))
            .connect(&url)
            .await
            .unwrap();
        let reader = PostgresPersistenceStore::with_table_prefix(reader_pool.clone(), &prefix);
        let trace = reader
            .load("wire-result-ack-loss")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(trace.events.len(), 3);
        assert_eq!(trace.events[0].outcome_kind, "Enter");
        assert_eq!(trace.events[0].step, 0);
        assert_eq!(
            trace.events[1].event_type,
            ranvier_runtime::persistence::PersistenceEventType::Intent
        );
        assert_eq!(trace.events[1].step, 1);
        assert_eq!(trace.events[2].event_type, ranvier_runtime::persistence::PersistenceEventType::Result);
        assert_eq!(trace.events[2].outcome_kind, "Next");
        assert_eq!(trace.events[2].step, 1);
        assert_eq!(
            trace.events[2].payload,
            Some(Outcome::<u32, String>::next(1).to_json_value())
        );
        assert_eq!(trace.completion, None);
        reader_pool.close().await;
        direct_pool.close().await;
        println!(
            "POSTGRES_TCP_RESULT_ACK_LOST dropped=ReadyForQuery-after-COMMIT-4 effects=1 stored-result-present completion=unset"
        );
    })
    .await
    .expect("isolated TCP result acknowledgement fixture must finish within 15 seconds");
}

#[tokio::test]
#[ignore = "requires explicitly isolated PostgreSQL fixture URL"]
async fn postgres_compensation_marker_ack_lost_on_tcp_reports_uncertainty() {
    use ranvier_core::prelude::*;
    use ranvier_runtime::persistence::{
        CompensationContext, CompensationHandle, CompensationHook, CompensationIdempotencyHandle,
        InMemoryPersistenceStore, PersistenceHandle, PersistenceTraceId,
        PostgresCompensationIdempotencyStore,
    };
    use ranvier_runtime::{EffectBoundary, ExecutionReport, PersistenceOperation};
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::net::{TcpListener, TcpStream};

    #[derive(Clone)]
    struct CountFault(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl Transition<u32, u32> for CountFault {
        type Error = String;
        type Resources = ();
        async fn run(&self, _: u32, _: &(), _: &mut Bus) -> Outcome<u32, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Outcome::Fault("business-fault".into())
        }
    }

    #[derive(Clone)]
    struct CountCompensation(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl CompensationHook for CountCompensation {
        async fn compensate(&self, _: CompensationContext) -> anyhow::Result<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        let url = std::env::var("RANVIER_ATOMICITY_POSTGRES_URL")
            .expect("explicit isolated database required");
        let database_options = url.parse::<PgConnectOptions>().unwrap();
        let upstream_host = database_options.get_host().to_owned();
        let upstream_port = database_options.get_port();
        let direct_pool = PgPoolOptions::new()
            .max_connections(3)
            .acquire_timeout(std::time::Duration::from_secs(3))
            .connect(&url)
            .await
            .unwrap();
        let prefix = format!("cmp_{}", uuid::Uuid::new_v4().simple());
        let trace_store = InMemoryPersistenceStore::new();
        let direct_marker_store =
            PostgresCompensationIdempotencyStore::with_table_prefix(direct_pool.clone(), &prefix);
        direct_marker_store.ensure_schema().await.unwrap();

        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let proxy_address = listener.local_addr().unwrap();
        let proxy_task = tokio::spawn(async move {
            let (client, _) = listener.accept().await?;
            let database = TcpStream::connect((upstream_host.as_str(), upstream_port)).await?;
            postgres_proxy_drop_ready_for_query_after(
                client,
                database,
                PostgresAckLossTarget::CompensationMarkerInsert,
            )
            .await
        });
        let proxy_options = database_options
            .host("127.0.0.1")
            .port(proxy_address.port())
            .ssl_mode(PgSslMode::Disable);
        let proxy_pool = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(3))
            .connect_with(proxy_options)
            .await
            .unwrap();
        let proxy_marker_store =
            PostgresCompensationIdempotencyStore::with_table_prefix(proxy_pool.clone(), &prefix);

        let effects = Arc::new(AtomicUsize::new(0));
        let compensation_calls = Arc::new(AtomicUsize::new(0));
        let axon = ranvier_runtime::Axon::<u32, u32, String, ()>::new(
            "PostgresWireCompensationMarkerAckLossFixture",
        )
        .then(CountFault(effects.clone()));
        let trace_id = "wire-compensation-marker-ack-loss";
        let mut bus = Bus::new();
        bus.insert(PersistenceTraceId::new(trace_id));
        bus.insert(PersistenceHandle::from_store(trace_store.clone()));
        bus.insert(CompensationHandle::from_hook(CountCompensation(
            compensation_calls.clone(),
        )));
        bus.insert(CompensationIdempotencyHandle::from_store(
            proxy_marker_store,
        ));

        let report = axon.execute(7, &(), &mut bus).await;
        let proxy_task_observation =
            tokio::time::timeout(std::time::Duration::from_secs(3), proxy_task)
                .await
                .expect("TCP proxy must observe the compensation marker insert response")
                .expect("TCP proxy task must not panic")
                .expect("TCP proxy must relay and drop the targeted backend frame");
        assert_eq!(
            proxy_task_observation,
            PostgresAckLossObservation {
                target: PostgresAckLossTarget::CompensationMarkerInsert,
                command_tag: "INSERT 0 1",
                transaction_status: b'I',
            }
        );
        assert!(matches!(
            &report,
            ExecutionReport::Unconfirmed {
                result: Outcome::Fault(message),
                operation: PersistenceOperation::CompensationWrite,
                effect: EffectBoundary::MayHaveRun,
                ..
            } if message == "business-fault"
        ), "unexpected compensation marker report: {report:?}");
        let cleanup = report
            .cleanup()
            .expect("external compensation cleanup must be reported");
        assert_eq!(cleanup.trace_id, trace_id);
        assert!(!cleanup.is_complete());
        assert_eq!(cleanup.steps.len(), 1);
        assert_eq!(
            cleanup.steps[0].outcome,
            ranvier_runtime::SagaRollbackOutcome::Fault
        );
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        assert_eq!(compensation_calls.load(Ordering::SeqCst), 1);
        proxy_pool.close().await;

        let marker_count: i64 = sqlx::query_scalar(&format!(
            "SELECT COUNT(*) FROM {prefix}_compensation_idempotency"
        ))
        .fetch_one(&direct_pool)
        .await
        .unwrap();
        assert_eq!(marker_count, 1, "the marker insert must have committed");
        direct_pool.close().await;
        println!(
            "POSTGRES_TCP_COMPENSATION_MARKER_ACK_LOST dropped=ReadyForQuery-after-INSERT-0-1 effect=MayHaveRun effects=1 hook_calls=1 marker_rows=1 cleanup=Fault trace_store=InMemory postgres_trace_store=false"
        );
    })
    .await
    .expect("isolated TCP compensation-marker acknowledgement fixture must finish within 15 seconds");
}

// Characterizes historical false-success rows written before completion was
// gated on terminal persistence. This is a safety observation, not acceptance
// that a missing terminal event should be treated as complete.
#[tokio::test]
#[ignore = "requires explicitly isolated PostgreSQL fixture URL"]
async fn postgres_completed_trace_without_terminal_requires_reconciliation() {
    use ranvier_core::prelude::*;
    use ranvier_runtime::persistence::{PersistenceHandle, PersistenceTraceId};
    use ranvier_runtime::{Axon, ExecutionReport};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[derive(Clone)]
    struct CountStep(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl Transition<u32, u32> for CountStep {
        type Error = String;
        type Resources = ();
        async fn run(&self, input: u32, _: &(), _: &mut Bus) -> Outcome<u32, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Outcome::next(input + 1)
        }
    }

    let url = std::env::var("RANVIER_ATOMICITY_POSTGRES_URL")
        .expect("explicit isolated database required");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(std::time::Duration::from_secs(3))
        .connect(&url)
        .await
        .unwrap();
    let prefix = format!("legacy_{}", uuid::Uuid::new_v4().simple());
    let store = PostgresPersistenceStore::with_table_prefix(pool.clone(), &prefix);
    store.ensure_schema().await.unwrap();

    let effects = Arc::new(AtomicUsize::new(0));
    let axon = Axon::<u32, u32, String, ()>::new("LegacyTerminalFixture")
        .then(CountStep(effects.clone()))
        .then(CountStep(effects.clone()));
    let trace_id = format!("legacy-success-{}", uuid::Uuid::new_v4().simple());
    let version = &axon.schematic().schema_version;

    let mut enter = event(&trace_id, 0, version);
    enter.outcome_kind = "Enter".into();
    store.append(enter).await.unwrap();
    let node_id = axon.schematic().nodes[0].id.clone();
    let mut intent = event(&trace_id, 1, version);
    intent.step = 0;
    intent.event_type = ranvier_runtime::persistence::PersistenceEventType::Intent;
    intent.node_id = Some(node_id.clone());
    intent.outcome_kind = "Intent".into();
    intent.payload = Some(serde_json::json!({ "input": 0 }));
    store.append(intent).await.unwrap();
    let mut result = event(&trace_id, 2, version);
    result.step = 0;
    result.event_type = ranvier_runtime::persistence::PersistenceEventType::Result;
    result.node_id = Some(node_id);
    result.outcome_kind = "Next".into();
    result.payload = Some(Outcome::<u32, String>::next(1).to_json_value());
    store.append(result).await.unwrap();
    sqlx::query(&format!(
        "UPDATE {prefix}_v2_state SET completion = 'success' WHERE trace_id = $1"
    ))
    .bind(&trace_id)
    .execute(&pool)
    .await
    .unwrap();
    let before = store.load(&trace_id).await.unwrap().unwrap();
    assert_eq!(before.events.len(), 3);
    assert_eq!(
        before
            .events
            .iter()
            .map(|stored| stored.step)
            .collect::<Vec<_>>(),
        vec![0, 0, 0]
    );
    assert_eq!(before.events[0].outcome_kind, "Enter");
    assert_eq!(before.events[1].outcome_kind, "Intent");
    assert_eq!(before.events[2].outcome_kind, "Next");

    let mut bus = Bus::new();
    bus.insert(PersistenceHandle::from_store(store.clone()));
    bus.insert(PersistenceTraceId::new(trace_id.clone()));
    assert!(matches!(
        axon.execute(0, &(), &mut bus).await,
        ExecutionReport::BlockedBeforeEffect {
            operation: ranvier_runtime::PersistenceOperation::Terminal,
            effect: ranvier_runtime::EffectBoundary::NotInvoked,
            ..
        }
    ));
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert_eq!(store.load(&trace_id).await.unwrap().unwrap(), before);
    println!(
        "POSTGRES_COMPLETED_WITHOUT_TERMINAL_RECONCILIATION completion-success terminal-event-missing effects=0 full-trace-unchanged"
    );
    pool.close().await;
}

// External owned-container rehearsal selects this worker in a new process per phase.
#[tokio::test]
async fn postgres_recovery_worker() {
    let Ok(mode) = std::env::var("RANVIER_RECOVERY_MODE") else {
        return;
    };
    let prefix = std::env::var("RANVIER_RECOVERY_PREFIX").unwrap();
    let snapshot = std::env::var("RANVIER_RECOVERY_SNAPSHOT").unwrap();
    let url = std::env::var("RANVIER_ATOMICITY_POSTGRES_URL").unwrap();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(3))
        .connect(&url)
        .await
        .unwrap();
    let store = PostgresPersistenceStore::with_table_prefix(pool.clone(), &prefix);
    match mode.as_str() {
        "seed" => {
            store.ensure_schema().await.unwrap();
            store.append(event("recovery", 0, "seed")).await.unwrap();
            store
                .append(event("recovery", 1, "acknowledged"))
                .await
                .unwrap();
            store
                .save_intervention(
                    "recovery",
                    Intervention {
                        target_node: "recovery-node".into(),
                        payload_override: Some(
                            serde_json::json!({"fixture": [1, true, "retained"]}),
                        ),
                        timestamp_ms: 1100,
                        expected_event_sequence: 2,
                    },
                )
                .await
                .unwrap();
            store
                .append(terminal_event("recovery", 2, 2, "acknowledged", "Next"))
                .await
                .unwrap();
            store
                .complete("recovery", CompletionState::Success)
                .await
                .unwrap();
            let trace = store.load("recovery").await.unwrap().unwrap();
            assert_eq!(
                trace.events,
                vec![
                    event("recovery", 0, "seed"),
                    event("recovery", 1, "acknowledged"),
                    terminal_event("recovery", 2, 2, "acknowledged", "Next")
                ]
            );
            assert_eq!(trace.schematic_version, "acknowledged");
            assert_eq!(trace.completion, Some(CompletionState::Success));
            assert_eq!(
                trace.interventions,
                vec![Intervention {
                    target_node: "recovery-node".into(),
                    payload_override: Some(serde_json::json!({"fixture": [1, true, "retained"]})),
                    timestamp_ms: 1100,
                    expected_event_sequence: 2,
                }]
            );
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&snapshot)
                .unwrap();
            serde_json::to_writer(&file, &trace).unwrap();
            file.sync_all().unwrap();
            println!("POSTGRES_RECOVERY_SEED acknowledged-event-terminal-intervention");
        }
        "check" => {
            let expected: ranvier_runtime::persistence::PersistedTrace =
                serde_json::from_reader(std::fs::File::open(&snapshot).unwrap()).unwrap();
            assert_eq!(store.load("recovery").await.unwrap(), Some(expected));
            println!("POSTGRES_RECOVERY_CHECK fresh-process full-snapshot-equality");
        }
        _ => panic!("unknown recovery worker mode"),
    }
    pool.close().await;
}

// Invoked only by the independent-process fixture, never accepted as evidence alone.
#[tokio::test]
async fn postgres_process_worker() {
    let Ok(prefix) = std::env::var("RANVIER_PROCESS_PREFIX") else {
        return;
    };
    let url = std::env::var("RANVIER_ATOMICITY_POSTGRES_URL").unwrap();
    let label = std::env::var("RANVIER_PROCESS_LABEL").unwrap();
    let step: u64 = std::env::var("RANVIER_PROCESS_STEP")
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(step, 1, "both workers contend for the same next sequence");
    let options = url.parse::<sqlx::postgres::PgConnectOptions>().unwrap();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(3))
        .connect_with(options.application_name(&label))
        .await
        .unwrap();
    let store = PostgresPersistenceStore::with_table_prefix(pool.clone(), &prefix);
    let before = store.load("processes").await.unwrap().unwrap();
    assert_eq!(before.events, vec![event("processes", 0, "seed")]);
    assert_eq!(before.schematic_version, "seed");
    assert_eq!(before.completion, None);
    assert!(before.interventions.is_empty());
    match store.append(event("processes", step, &label)).await {
        Ok(()) => println!("POSTGRES_PROCESS_APPEND winner={label}"),
        Err(error) if error.to_string().contains("expected event sequence") => {
            println!("POSTGRES_PROCESS_APPEND stale={label}");
        }
        Err(error) => panic!("unexpected concurrent writer failure: {error:#}"),
    }
    pool.close().await;
}

struct FixtureProcess(std::process::Child);

impl FixtureProcess {
    fn start(prefix: &str, label: &str, step: u64) -> Self {
        Self(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "postgres_process_worker", "--nocapture"])
                .env("RANVIER_PROCESS_PREFIX", prefix)
                .env("RANVIER_PROCESS_LABEL", label)
                .env("RANVIER_PROCESS_STEP", step.to_string())
                .spawn()
                .unwrap(),
        )
    }

    async fn finish(&mut self) -> anyhow::Result<std::process::ExitStatus> {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(status) = self.0.try_wait()? {
                    return Ok(status);
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("fixture child exceeded exit deadline"))?
    }
}

impl Drop for FixtureProcess {
    fn drop(&mut self) {
        // Retain ownership through assertions/timeouts; kill and reap only our child.
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

#[tokio::test]
#[ignore = "requires explicitly isolated PostgreSQL fixture URL"]
async fn postgres_killed_waiting_process_preserves_seed_and_allows_fresh_writer() {
    let url = std::env::var("RANVIER_ATOMICITY_POSTGRES_URL").unwrap();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(3)
        .acquire_timeout(std::time::Duration::from_secs(3))
        .connect(&url)
        .await
        .unwrap();
    let prefix = format!("killed_{}", uuid::Uuid::new_v4().simple());
    let state_table = format!("{prefix}_v2_state");
    let store = PostgresPersistenceStore::with_table_prefix(pool.clone(), &prefix);
    store.ensure_schema().await.unwrap();
    store.append(event("processes", 0, "seed")).await.unwrap();
    let seed = store.load("processes").await.unwrap().unwrap();
    let mut holder = pool.begin().await.unwrap();
    sqlx::query(&format!(
        "SELECT trace_id FROM {state_table} WHERE trace_id = 'processes' FOR UPDATE"
    ))
    .execute(&mut *holder)
    .await
    .unwrap();
    let killed_label = format!("killed_client_{}", uuid::Uuid::new_v4().simple());
    let mut child = FixtureProcess::start(&prefix, &killed_label, 1);
    let waited = wait_for_row_lock(&pool, &killed_label, "transactionid", &state_table).await;
    let backend_pid: i32 =
        sqlx::query_scalar("SELECT pid FROM pg_stat_activity WHERE application_name = $1")
            .bind(&killed_label)
            .fetch_one(&pool)
            .await
            .unwrap();
    let child_pid = child.0.id();
    let killed = child.0.kill();
    let exit = child.finish().await;
    drop(child);
    // A backend blocked in SQL need not observe TCP disconnect until the lock releases.
    let released = holder.commit().await;
    let disappeared = wait_for_backend_exit(&pool, &killed_label).await;
    waited.unwrap();
    killed.unwrap();
    let exit = exit.unwrap();
    assert!(
        !exit.success(),
        "killed child must not report successful append"
    );
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            exit.signal(),
            Some(9),
            "must observe actual SIGKILL, not worker assertion failure"
        );
    }
    released.unwrap();
    disappeared.unwrap();
    assert_eq!(store.load("processes").await.unwrap(), Some(seed.clone()));
    let fresh_label = format!("fresh_client_{}", uuid::Uuid::new_v4().simple());
    let mut fresh = FixtureProcess::start(&prefix, &fresh_label, 1);
    let fresh_exit = fresh.finish().await;
    drop(fresh);
    assert!(fresh_exit.unwrap().success());
    wait_for_backend_exit(&pool, &fresh_label).await.unwrap();
    let mut expected = seed;
    expected.schematic_version = fresh_label.clone();
    expected.events.push(event("processes", 1, &fresh_label));
    assert_eq!(store.load("processes").await.unwrap(), Some(expected));
    pool.close().await;
    println!("POSTGRES_KILLED_PID child={child_pid} backend={backend_pid} status={exit}");
    println!(
        "POSTGRES_KILL_PASS observed-row-wait killed-reaped backend-gone seed-intact fresh-process-append"
    );
}

async fn wait_for_backend_exit(pool: &sqlx::PgPool, label: &str) -> anyhow::Result<()> {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let present: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE application_name = $1)",
            )
            .bind(label)
            .fetch_one(pool)
            .await?;
            if !present {
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("owned client backend did not disappear"))?
}

#[tokio::test]
#[ignore = "requires explicitly isolated PostgreSQL fixture URL"]
async fn postgres_independent_process_writers_reject_stale_sequence() {
    let url = std::env::var("RANVIER_ATOMICITY_POSTGRES_URL").unwrap();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(3)
        .acquire_timeout(std::time::Duration::from_secs(3))
        .connect(&url)
        .await
        .unwrap();
    let prefix = format!("process_{}", uuid::Uuid::new_v4().simple());
    let state_table = format!("{prefix}_v2_state");
    let store = PostgresPersistenceStore::with_table_prefix(pool.clone(), &prefix);
    store.ensure_schema().await.unwrap();
    store.append(event("processes", 0, "seed")).await.unwrap();
    let mut holder = pool.begin().await.unwrap();
    sqlx::query(&format!(
        "SELECT trace_id FROM {state_table} WHERE trace_id = 'processes' FOR UPDATE"
    ))
    .execute(&mut *holder)
    .await
    .unwrap();
    let first_label = format!("proc_first_{}", uuid::Uuid::new_v4().simple());
    let second_label = format!("proc_second_{}", uuid::Uuid::new_v4().simple());
    let mut first = FixtureProcess::start(&prefix, &first_label, 1);
    let first_wait = wait_for_row_lock(&pool, &first_label, "transactionid", &state_table).await;
    let mut second = FixtureProcess::start(&prefix, &second_label, 1);
    assert_ne!(first.0.id(), second.0.id());
    assert_ne!(first.0.id(), std::process::id());
    assert_ne!(second.0.id(), std::process::id());
    println!(
        "POSTGRES_PROCESS_PIDS parent={} first={} second={}",
        std::process::id(),
        first.0.id(),
        second.0.id()
    );
    let second_wait = wait_for_row_lock(&pool, &second_label, "tuple", &state_table).await;
    let released = holder.commit().await;
    let first_exit = first.finish().await;
    let second_exit = second.finish().await;
    // Drop/reap both before evaluating results or reading the final trace.
    drop(first);
    drop(second);
    first_wait.unwrap();
    second_wait.unwrap();
    released.unwrap();
    assert!(first_exit.unwrap().success());
    assert!(second_exit.unwrap().success());
    let trace = store.load("processes").await.unwrap().unwrap();
    assert_eq!(trace.events.len(), 2);
    assert_eq!(trace.events[0], event("processes", 0, "seed"));
    assert_eq!(trace.events[1].event_sequence, 1);
    assert!(trace.events[1].schematic_version.starts_with("proc_"));
    assert_eq!(trace.completion, None);
    pool.close().await;
    println!("POSTGRES_PROCESS_PASS independent-pids same-seed row-queue one-next-sequence-winner");
}

#[tokio::test]
#[ignore = "requires explicitly isolated PostgreSQL fixture URL"]
async fn postgres_rejected_append_preserves_snapshot_and_rolls_back_insert_failure() {
    let url = std::env::var("RANVIER_ATOMICITY_POSTGRES_URL")
        .expect("explicit isolated database required");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(std::time::Duration::from_secs(3))
        .connect(&url)
        .await
        .unwrap();
    let prefix = format!("atomic_{}", uuid::Uuid::new_v4().simple());
    let store = PostgresPersistenceStore::with_table_prefix(pool.clone(), &prefix);
    store.ensure_schema().await.unwrap();
    store.append(event("existing", 0, "initial")).await.unwrap();
    let baseline = store.load("existing").await.unwrap();
    let mut wrong = event("existing", 1, "wrong-circuit-version");
    wrong.circuit = "OtherCircuit".into();
    assert!(store.append(wrong).await.is_err());
    assert_eq!(
        store.load("existing").await.unwrap(),
        baseline,
        "wrong circuit must not mutate state"
    );
    assert!(
        store
            .append({
                let mut stale = event("existing", 1, "duplicate-version");
                stale.event_sequence = 0;
                stale
            })
            .await
            .is_err()
    );
    assert_eq!(
        store.load("existing").await.unwrap(),
        baseline,
        "event insertion failure must roll back version"
    );
    let mut overflow = event("new-overflow", u64::MAX, "overflow-version");
    assert!(store.append(overflow.clone()).await.is_err());
    assert!(
        store.load("new-overflow").await.unwrap().is_none(),
        "failed first append must not leave state"
    );
    overflow.trace_id = "new-timestamp".into();
    overflow.event_sequence = 0;
    overflow.step = 1;
    overflow.timestamp_ms = u64::MAX;
    assert!(store.append(overflow).await.is_err());
    assert!(store.load("new-timestamp").await.unwrap().is_none());
    store
        .append(event("existing", 1, "accepted-version"))
        .await
        .unwrap();
    let accepted = store.load("existing").await.unwrap().unwrap();
    assert_eq!(accepted.schematic_version, "accepted-version");
    assert_eq!(accepted.events.len(), 2);
    assert!(
        store
            .complete("existing", CompletionState::Success)
            .await
            .is_err(),
        "completion cannot be asserted before a terminal event is recorded"
    );
    assert_eq!(
        store.load("existing").await.unwrap(),
        Some(accepted.clone())
    );
    store
        .append(terminal_event("existing", 2, 2, "accepted-version", "Next"))
        .await
        .unwrap();
    store
        .complete("existing", CompletionState::Success)
        .await
        .unwrap();
    let completed = store.load("existing").await.unwrap();
    assert!(
        store
            .append(event("existing", 3, "late-version"))
            .await
            .is_err()
    );
    assert_eq!(store.load("existing").await.unwrap(), completed);
    sqlx::query(&format!("ALTER TABLE {prefix}_v2_events ADD CONSTRAINT fixture_reject CHECK (outcome_kind <> 'RejectFixture')"))
        .execute(&pool).await.unwrap();
    let mut rejected_insert = event("new-insert-failure", 1, "new-version");
    rejected_insert.event_sequence = 0;
    rejected_insert.outcome_kind = "RejectFixture".into();
    assert!(store.append(rejected_insert).await.is_err());
    assert!(
        store.load("new-insert-failure").await.unwrap().is_none(),
        "first event insert failure must roll back state creation"
    );
    store.append(event("competing", 0, "seed")).await.unwrap();
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let mut writers = Vec::new();
    for writer_id in ["writer-a", "writer-b"] {
        let store = store.clone();
        let barrier = barrier.clone();
        writers.push(tokio::spawn(async move {
            let before = store.load("competing").await.unwrap().unwrap();
            assert_eq!(before.events.len(), 1);
            tokio::time::timeout(std::time::Duration::from_secs(3), barrier.wait())
                .await
                .unwrap();
            store.append(event("competing", 1, writer_id)).await
        }));
    }
    let mut results = Vec::new();
    for mut writer in writers {
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), &mut writer).await;
        if result.is_err() {
            writer.abort();
            let _ = writer.await;
        }
        results.push(result);
    }
    let results: Vec<_> = results
        .into_iter()
        .map(|result| result.unwrap().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
    let trace = store.load("competing").await.unwrap().unwrap();
    assert_eq!(trace.events.len(), 2);
    assert_eq!(trace.events[0], event("competing", 0, "seed"));
    assert!(matches!(
        trace.events[1].schematic_version.as_str(),
        "writer-a" | "writer-b"
    ));
    pool.close().await;
    println!(
        "POSTGRES_ATOMICITY_PASS wrong-circuit stale-sequence overflow accepted completed first-insert-rollback concurrent-sequence-conflict"
    );
}

#[tokio::test]
#[ignore = "requires explicitly isolated PostgreSQL fixture URL"]
async fn postgres_load_reads_one_snapshot_across_concurrent_append() {
    let url = std::env::var("RANVIER_ATOMICITY_POSTGRES_URL")
        .expect("explicit isolated database required");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(std::time::Duration::from_secs(3))
        .connect(&url)
        .await
        .unwrap();
    let prefix = format!("snapshot_{}", uuid::Uuid::new_v4().simple());
    let reader_prefix = format!("reader_{}", uuid::Uuid::new_v4().simple());
    let key = (uuid::Uuid::new_v4().as_u128() & i64::MAX as u128) as i64;
    let writer = PostgresPersistenceStore::with_table_prefix(pool.clone(), &prefix);
    writer.ensure_schema().await.unwrap();
    writer
        .append(event("snapshot", 0, "old-version"))
        .await
        .unwrap();
    let before = writer.load("snapshot").await.unwrap().unwrap();
    // The state statement takes its MVCC snapshot before waiting on this
    // fixture-only lock. The production writer uses the real backing tables.
    sqlx::query(&format!("CREATE VIEW {reader_prefix}_v2_state AS SELECT * FROM {prefix}_v2_state WHERE pg_advisory_xact_lock({key}) IS NOT NULL"))
        .execute(&pool).await.unwrap();
    for suffix in ["v2_events", "v2_interventions"] {
        sqlx::query(&format!(
            "CREATE VIEW {reader_prefix}_{suffix} AS SELECT * FROM {prefix}_{suffix}"
        ))
        .execute(&pool)
        .await
        .unwrap();
    }
    let options = url
        .parse::<sqlx::postgres::PgConnectOptions>()
        .unwrap()
        .application_name(&reader_prefix);
    let reader_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    let reader = PostgresPersistenceStore::with_table_prefix(reader_pool.clone(), &reader_prefix);
    let subsequent_reader = reader.clone();
    let mut holder = pool.acquire().await.unwrap();
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(key)
        .execute(&mut *holder)
        .await
        .unwrap();
    let mut loading = tokio::spawn(async move { reader.load("snapshot").await });
    let waiting = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let blocked: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_locks l JOIN pg_stat_activity a ON a.pid = l.pid WHERE l.locktype = 'advisory' AND NOT l.granted AND a.application_name = $1)")
                .bind(&reader_prefix).fetch_one(&pool).await.unwrap();
            if blocked { break; }
            tokio::task::yield_now().await;
        }
    }).await;
    let appended = if waiting.is_ok() {
        writer.append(event("snapshot", 1, "new-version")).await
    } else {
        Err(anyhow::anyhow!(
            "reader did not reach actual state-query barrier"
        ))
    };
    let intervention = Intervention {
        target_node: "review-node".into(),
        payload_override: Some(serde_json::json!({"case": "snapshot-review", "approved": false})),
        timestamp_ms: 1001,
        expected_event_sequence: 2,
    };
    let intervened = if appended.is_ok() {
        writer
            .save_intervention("snapshot", intervention.clone())
            .await
    } else {
        Err(anyhow::anyhow!("append failed before intervention fixture"))
    };
    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(key)
        .execute(&mut *holder)
        .await
        .unwrap();
    drop(holder);
    let loaded = tokio::time::timeout(std::time::Duration::from_secs(3), &mut loading).await;
    if loaded.is_err() {
        loading.abort();
        let _ = loading.await;
    }
    let next = subsequent_reader.load("snapshot").await;
    let absent = subsequent_reader.load("missing").await;
    reader_pool.close().await;
    let after = writer.load("snapshot").await.unwrap().unwrap();
    pool.close().await;
    waiting.expect("actual reader advisory-lock wait required");
    appended.unwrap();
    intervened.unwrap();
    assert_eq!(
        next.unwrap(),
        Some(after.clone()),
        "next load must take a fresh snapshot"
    );
    assert!(absent.unwrap().is_none());
    assert_eq!(after.interventions, vec![intervention]);
    assert_eq!(after.schematic_version, "new-version");
    assert_eq!(after.events.len(), 2);
    assert_eq!(
        loaded.unwrap().unwrap().unwrap().unwrap(),
        before,
        "one load must not mix pre-append state with post-append events"
    );
    println!(
        "POSTGRES_SNAPSHOT_PASS actual-state-query-barrier concurrent-append intervention fresh-load absent one-snapshot"
    );
}

#[tokio::test]
#[ignore = "requires explicitly isolated PostgreSQL fixture URL"]
async fn postgres_completion_before_terminal_append_does_not_complete() {
    verify_append_completion_order(true).await;
}

#[tokio::test]
#[ignore = "requires explicitly isolated PostgreSQL fixture URL"]
async fn postgres_terminal_append_before_completion_commits_trace() {
    verify_append_completion_order(false).await;
}

async fn wait_for_row_lock(
    pool: &sqlx::PgPool,
    label: &str,
    lock_kind: &str,
    state_table: &str,
) -> anyhow::Result<()> {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let blocked: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_locks l JOIN pg_stat_activity a ON a.pid = l.pid WHERE NOT l.granted AND l.locktype = $2 AND a.application_name = $1 AND a.wait_event_type = 'Lock' AND a.query LIKE $3)")
                .bind(label).bind(lock_kind).bind(format!("%{state_table}%")).fetch_one(pool).await?;
            if blocked {
                println!("POSTGRES_ROW_WAIT kind={lock_kind} table={state_table} label={label}");
                return Ok::<(), anyhow::Error>(());
            }
            tokio::task::yield_now().await;
        }
    }).await?
}

async fn verify_append_completion_order(completion_first: bool) {
    let url = std::env::var("RANVIER_ATOMICITY_POSTGRES_URL")
        .expect("explicit isolated database required");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(std::time::Duration::from_secs(3))
        .connect(&url)
        .await
        .unwrap();
    let prefix = format!("order_{}", uuid::Uuid::new_v4().simple());
    let store = PostgresPersistenceStore::with_table_prefix(pool.clone(), &prefix);
    store.ensure_schema().await.unwrap();
    let initial = event("ordered", 0, "initial");
    store.append(initial.clone()).await.unwrap();
    let terminal = terminal_event("ordered", 1, 1, "appended", "Next");
    let first_label = format!("first_{}", uuid::Uuid::new_v4().simple());
    let second_label = format!("second_{}", uuid::Uuid::new_v4().simple());
    let options = url.parse::<sqlx::postgres::PgConnectOptions>().unwrap();
    let first_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options.clone().application_name(&first_label))
        .await
        .unwrap();
    let second_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options.application_name(&second_label))
        .await
        .unwrap();
    let first_store = PostgresPersistenceStore::with_table_prefix(first_pool.clone(), &prefix);
    let second_store = PostgresPersistenceStore::with_table_prefix(second_pool.clone(), &prefix);
    let mut holder = pool.begin().await.unwrap();
    sqlx::query(&format!(
        "SELECT trace_id FROM {prefix}_v2_state WHERE trace_id = 'ordered' FOR UPDATE"
    ))
    .execute(&mut *holder)
    .await
    .unwrap();
    let first_terminal = terminal.clone();
    let mut first = tokio::spawn(async move {
        if completion_first {
            first_store
                .complete("ordered", CompletionState::Success)
                .await
        } else {
            first_store.append(first_terminal).await
        }
    });
    let first_wait = wait_for_row_lock(
        &pool,
        &first_label,
        "transactionid",
        &format!("{prefix}_v2_state"),
    )
    .await;
    let second_terminal = terminal.clone();
    let mut second = tokio::spawn(async move {
        if completion_first {
            second_store.append(second_terminal).await
        } else {
            second_store
                .complete("ordered", CompletionState::Success)
                .await
        }
    });
    let second_wait =
        wait_for_row_lock(&pool, &second_label, "tuple", &format!("{prefix}_v2_state")).await;
    let released = holder.commit().await;
    let first_result = tokio::time::timeout(std::time::Duration::from_secs(3), &mut first).await;
    if first_result.is_err() {
        first.abort();
        let _ = first.await;
    }
    let second_result = tokio::time::timeout(std::time::Duration::from_secs(3), &mut second).await;
    if second_result.is_err() {
        second.abort();
        let _ = second.await;
    }
    first_pool.close().await;
    second_pool.close().await;
    first_wait.expect("first actual operation must wait on holder transaction");
    second_wait.expect("second actual operation must queue on tuple behind first");
    released.unwrap();
    let first_result = first_result
        .expect("first operation must finish after row release")
        .expect("first operation task must not panic");
    let second_result = second_result
        .expect("second operation must finish after row release")
        .expect("second operation task must not panic");
    if completion_first {
        let error = first_result.expect_err("completion before terminal must fail closed");
        assert!(error.to_string().contains("terminal event"), "{error:#}");
        second_result.expect("terminal append after rejected completion must succeed");
    } else {
        first_result.expect("terminal append before completion must succeed");
        second_result.expect("completion after terminal append must succeed");
    }

    let trace = store.load("ordered").await.unwrap().unwrap();
    assert_eq!(trace.schematic_version, "appended");
    assert_eq!(trace.events, vec![initial, terminal]);
    assert_eq!(
        trace.completion,
        if completion_first {
            None
        } else {
            Some(CompletionState::Success)
        }
    );

    let late = store
        .append(event("ordered", 2, "rejected-late-version"))
        .await;
    let after_late = store.load("ordered").await;
    let late_error = late.unwrap_err().to_string();
    if completion_first {
        assert!(
            late_error.contains("after a terminal event"),
            "{late_error}"
        );
    } else {
        assert!(late_error.contains("already completed"), "{late_error}");
    }
    assert_eq!(
        after_late.unwrap(),
        Some(trace.clone()),
        "late rejected append must preserve full terminal snapshot"
    );
    pool.close().await;
    println!(
        "POSTGRES_ORDER_PASS completion_first={completion_first} terminal-append-completion actual-transactionid-and-tuple-waits"
    );
}
