#![cfg(feature = "persistence-postgres")]
use ranvier_runtime::persistence::{
    CompletionState, PersistenceEnvelope, PersistenceStore, PostgresPersistenceStore,
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
    pool.close().await;
    println!("POSTGRES_ATOMICITY_PASS wrong-circuit duplicate overflow accepted completed");
}
