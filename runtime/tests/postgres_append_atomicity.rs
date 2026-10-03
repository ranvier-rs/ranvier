#![cfg(feature = "persistence-postgres")]
use ranvier_runtime::persistence::{
    CompletionState, Intervention, PersistenceEnvelope, PersistenceStore, PostgresPersistenceStore,
};

fn event(trace: &str, step: u64, version: &str) -> PersistenceEnvelope {
    PersistenceEnvelope {
        trace_id: trace.into(),
        circuit: "AtomicFixture".into(),
        schematic_version: version.into(),
        step,
        node_id: None,
        outcome_kind: "Next".into(),
        timestamp_ms: 1000,
        payload_hash: None,
        payload: None,
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
            let mut seed = event(&trace_id, 1, &axon.schematic().schema_version);
            seed.payload = Some(serde_json::Value::Null);
            store.append(seed).await.unwrap();
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

// Retains terminal collision defect evidence while requiring the versioned
// caller report to expose uncertainty. Missing terminal data is not acceptance.
#[tokio::test]
#[ignore = "requires explicitly isolated PostgreSQL fixture URL"]
async fn postgres_executor_terminal_step_collision_is_observable() {
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
        ranvier_runtime::ExecutionReport::Unconfirmed {
            result: Outcome::Next(2),
            operation: ranvier_runtime::PersistenceOperation::Terminal,
            effect: ranvier_runtime::EffectBoundary::MayHaveRun,
            ..
        }
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let trace = store.load("terminal-collision").await.unwrap().unwrap();
    assert_eq!(trace.completion, None);
    assert_eq!(
        trace.events.iter().map(|e| e.step).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(trace.events[0].outcome_kind, "Enter");
    // PostgreSQL currently does not round-trip node_id. Distinguish the
    // attempted terminal at step1 by payload, not by absent node metadata.
    let first = Outcome::<u32, String>::next(1).to_json_value();
    let terminal = Outcome::<u32, String>::next(2).to_json_value();
    assert_ne!(first, terminal);
    assert_eq!(trace.events[1].outcome_kind, "Next");
    assert_eq!(trace.events[1].payload, Some(first));
    assert_eq!(trace.events[2].payload, Some(terminal));
    // An incomplete cursor beyond the graph must block rather than replay.
    let reentry = axon.execute(0, &(), &mut bus).await;
    assert!(matches!(
        reentry,
        ranvier_runtime::ExecutionReport::BlockedBeforeEffect {
            operation: ranvier_runtime::PersistenceOperation::Terminal,
            effect: ranvier_runtime::EffectBoundary::NotInvoked,
            ..
        }
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        store.load("terminal-collision").await.unwrap().unwrap(),
        trace
    );
    println!(
        "POSTGRES_TERMINAL_COLLISION_REENTRY_BLOCKED effects=2 completion-unset full-trace-unchanged"
    );
    pool.close().await;
    println!(
        "POSTGRES_TERMINAL_COLLISION_REPRODUCED effects=2 caller-next=2 durability-unconfirmed-terminal completion-unset terminal-event-missing"
    );
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
                    },
                )
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
                    event("recovery", 1, "acknowledged")
                ]
            );
            assert_eq!(trace.schematic_version, "acknowledged");
            assert_eq!(trace.completion, Some(CompletionState::Success));
            assert_eq!(
                trace.interventions,
                vec![Intervention {
                    target_node: "recovery-node".into(),
                    payload_override: Some(serde_json::json!({"fixture": [1, true, "retained"]})),
                    timestamp_ms: 1100
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
    assert!(matches!(step, 1 | 2));
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
    store
        .append(event("processes", step, "concurrent"))
        .await
        .unwrap();
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
    let state_table = format!("{prefix}_state");
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
    expected.schematic_version = "concurrent".into();
    expected.events.push(event("processes", 1, "concurrent"));
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
async fn postgres_independent_process_writers_preserve_both_events() {
    let url = std::env::var("RANVIER_ATOMICITY_POSTGRES_URL").unwrap();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(3)
        .acquire_timeout(std::time::Duration::from_secs(3))
        .connect(&url)
        .await
        .unwrap();
    let prefix = format!("process_{}", uuid::Uuid::new_v4().simple());
    let state_table = format!("{prefix}_state");
    let store = PostgresPersistenceStore::with_table_prefix(pool.clone(), &prefix);
    store.ensure_schema().await.unwrap();
    store.append(event("processes", 0, "seed")).await.unwrap();
    let mut expected = store.load("processes").await.unwrap().unwrap();
    expected.schematic_version = "concurrent".into();
    expected.events.extend([
        event("processes", 1, "concurrent"),
        event("processes", 2, "concurrent"),
    ]);
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
    let mut second = FixtureProcess::start(&prefix, &second_label, 2);
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
    assert_eq!(
        trace.events,
        vec![
            event("processes", 0, "seed"),
            event("processes", 1, "concurrent"),
            event("processes", 2, "concurrent")
        ]
    );
    assert_eq!(trace.schematic_version, "concurrent");
    assert_eq!(trace.completion, None);
    assert_eq!(
        trace, expected,
        "all state, event and intervention fields must match"
    );
    pool.close().await;
    println!("POSTGRES_PROCESS_PASS independent-pids same-seed row-queue exact-events");
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
    store.append(event("existing", 1, "initial")).await.unwrap();
    let baseline = store.load("existing").await.unwrap();
    let mut wrong = event("existing", 2, "wrong-circuit-version");
    wrong.circuit = "OtherCircuit".into();
    assert!(store.append(wrong).await.is_err());
    assert_eq!(
        store.load("existing").await.unwrap(),
        baseline,
        "wrong circuit must not mutate state"
    );
    assert!(
        store
            .append(event("existing", 1, "duplicate-version"))
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
    overflow.step = 1;
    overflow.timestamp_ms = u64::MAX;
    assert!(store.append(overflow).await.is_err());
    assert!(store.load("new-timestamp").await.unwrap().is_none());
    store
        .append(event("existing", 2, "accepted-version"))
        .await
        .unwrap();
    let accepted = store.load("existing").await.unwrap().unwrap();
    assert_eq!(accepted.schematic_version, "accepted-version");
    assert_eq!(accepted.events.len(), 2);
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
    sqlx::query(&format!("ALTER TABLE {prefix}_events ADD CONSTRAINT fixture_reject CHECK (outcome_kind <> 'RejectFixture')"))
        .execute(&pool).await.unwrap();
    let mut rejected_insert = event("new-insert-failure", 1, "new-version");
    rejected_insert.outcome_kind = "RejectFixture".into();
    assert!(store.append(rejected_insert).await.is_err());
    assert!(
        store.load("new-insert-failure").await.unwrap().is_none(),
        "first event insert failure must roll back state creation"
    );
    store.append(event("competing", 0, "seed")).await.unwrap();
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let mut writers = Vec::new();
    for step in [1, 2] {
        let store = store.clone();
        let barrier = barrier.clone();
        writers.push(tokio::spawn(async move {
            let before = store.load("competing").await.unwrap().unwrap();
            assert_eq!(before.events.len(), 1);
            tokio::time::timeout(std::time::Duration::from_secs(3), barrier.wait())
                .await
                .unwrap();
            store.append(event("competing", step, "concurrent")).await
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
    for result in results {
        result.unwrap().unwrap().unwrap();
    }
    let trace = store.load("competing").await.unwrap().unwrap();
    assert_eq!(
        trace.events,
        vec![
            event("competing", 0, "seed"),
            event("competing", 1, "concurrent"),
            event("competing", 2, "concurrent")
        ]
    );
    pool.close().await;
    println!(
        "POSTGRES_ATOMICITY_PASS wrong-circuit duplicate overflow accepted completed first-insert-rollback competing-writers"
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
        .append(event("snapshot", 1, "old-version"))
        .await
        .unwrap();
    let before = writer.load("snapshot").await.unwrap().unwrap();
    // The state statement takes its MVCC snapshot before waiting on this
    // fixture-only lock. The production writer uses the real backing tables.
    sqlx::query(&format!("CREATE VIEW {reader_prefix}_state AS SELECT * FROM {prefix}_state WHERE pg_advisory_xact_lock({key}) IS NOT NULL"))
        .execute(&pool).await.unwrap();
    for suffix in ["events", "interventions"] {
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
        writer.append(event("snapshot", 2, "new-version")).await
    } else {
        Err(anyhow::anyhow!(
            "reader did not reach actual state-query barrier"
        ))
    };
    let intervention = Intervention {
        target_node: "review-node".into(),
        payload_override: Some(serde_json::json!({"case": "snapshot-review", "approved": false})),
        timestamp_ms: 1001,
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
async fn postgres_completion_before_append_rejects_late_event() {
    verify_append_completion_order(true).await;
}

#[tokio::test]
#[ignore = "requires explicitly isolated PostgreSQL fixture URL"]
async fn postgres_append_before_completion_preserves_event_and_terminal() {
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
    store.append(event("ordered", 0, "initial")).await.unwrap();
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
        "SELECT trace_id FROM {prefix}_state WHERE trace_id = 'ordered' FOR UPDATE"
    ))
    .execute(&mut *holder)
    .await
    .unwrap();
    let mut first = tokio::spawn(async move {
        if completion_first {
            first_store
                .complete("ordered", CompletionState::Success)
                .await
        } else {
            first_store.append(event("ordered", 1, "appended")).await
        }
    });
    let first_wait = wait_for_row_lock(
        &pool,
        &first_label,
        "transactionid",
        &format!("{prefix}_state"),
    )
    .await;
    let mut second = tokio::spawn(async move {
        if completion_first {
            second_store.append(event("ordered", 1, "appended")).await
        } else {
            second_store
                .complete("ordered", CompletionState::Success)
                .await
        }
    });
    let second_wait =
        wait_for_row_lock(&pool, &second_label, "tuple", &format!("{prefix}_state")).await;
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
    let final_trace = store.load("ordered").await;
    let late = store
        .append(event("ordered", 2, "rejected-late-version"))
        .await;
    let after_late = store.load("ordered").await;
    pool.close().await;
    first_wait.expect("first actual operation must wait on holder transaction");
    second_wait.expect("second actual operation must queue on tuple behind first");
    released.unwrap();
    first_result.unwrap().unwrap().unwrap();
    let second_result = second_result.unwrap().unwrap();
    let trace = final_trace.unwrap().unwrap();
    assert_eq!(trace.completion, Some(CompletionState::Success));
    assert!(late.unwrap_err().to_string().contains("already completed"));
    assert_eq!(
        after_late.unwrap(),
        Some(trace.clone()),
        "late rejected append must preserve full terminal snapshot"
    );
    if completion_first {
        assert!(
            second_result
                .unwrap_err()
                .to_string()
                .contains("already completed")
        );
        assert_eq!(trace.schematic_version, "initial");
        assert_eq!(trace.events, vec![event("ordered", 0, "initial")]);
    } else {
        second_result.unwrap();
        assert_eq!(trace.schematic_version, "appended");
        assert_eq!(
            trace.events,
            vec![
                event("ordered", 0, "initial"),
                event("ordered", 1, "appended")
            ]
        );
    }
    println!(
        "POSTGRES_ORDER_PASS completion_first={completion_first} actual-transactionid-and-tuple-waits"
    );
}
