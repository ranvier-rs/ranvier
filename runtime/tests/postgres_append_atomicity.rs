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
) -> anyhow::Result<()> {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let blocked: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_locks l JOIN pg_stat_activity a ON a.pid = l.pid WHERE NOT l.granted AND l.locktype = $2 AND a.application_name = $1 AND a.wait_event_type = 'Lock')")
                .bind(label).bind(lock_kind).fetch_one(pool).await?;
            if blocked { return Ok::<(), anyhow::Error>(()); }
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
    let first_wait = wait_for_row_lock(&pool, &first_label, "transactionid").await;
    let mut second = tokio::spawn(async move {
        if completion_first {
            second_store.append(event("ordered", 1, "appended")).await
        } else {
            second_store
                .complete("ordered", CompletionState::Success)
                .await
        }
    });
    let second_wait = wait_for_row_lock(&pool, &second_label, "tuple").await;
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
    pool.close().await;
    first_wait.expect("first actual operation must wait on holder transaction");
    second_wait.expect("second actual operation must queue on tuple behind first");
    released.unwrap();
    first_result.unwrap().unwrap().unwrap();
    let second_result = second_result.unwrap().unwrap();
    let trace = final_trace.unwrap().unwrap();
    assert_eq!(trace.completion, Some(CompletionState::Success));
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
