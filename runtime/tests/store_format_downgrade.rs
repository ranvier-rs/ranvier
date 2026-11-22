//! P2-6 G7: downgrade to the released v0.51.x and upgrade back
//! (D-STORE-FORMAT F7 b). Run in three phases against disposable stores:
//!
//! 1. `downgrade_seed` (this build) initializes the stores under
//!    `RANVIER_DOWNGRADE_PREFIX` and writes version 2 trace `dg-v2`.
//! 2. v0.51.0 sources load `dg-v2` (not visible) and write version 1 trace
//!    `dg-v1` under the same prefix.
//! 3. `downgrade_check` (this build): PostgreSQL refuses to start with
//!    `StoreFormatError::LegacyData` (version 1 tables are checked on every
//!    start). Redis starts, because the keyspace is scanned only while the
//!    store has no marker; `dg-v1` stays invisible and `dg-v2` is intact.
//!
//! Both tests are ignored; they need `RANVIER_PERSISTENCE_POSTGRES_URL`,
//! `RANVIER_PERSISTENCE_REDIS_URL` and `RANVIER_DOWNGRADE_PREFIX`.

#![cfg(all(feature = "persistence-postgres", feature = "persistence-redis"))]

use ranvier_runtime::persistence::*;

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} required"))
}

async fn stores() -> (String, PostgresPersistenceStore, RedisPersistenceStore) {
    let prefix = env("RANVIER_DOWNGRADE_PREFIX");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&env("RANVIER_PERSISTENCE_POSTGRES_URL"))
        .await
        .unwrap();
    let manager = redis::aio::ConnectionManager::new(
        redis::Client::open(env("RANVIER_PERSISTENCE_REDIS_URL").as_str()).unwrap(),
    )
    .await
    .unwrap();
    (
        prefix.clone(),
        PostgresPersistenceStore::with_table_prefix(pool, prefix.clone()),
        RedisPersistenceStore::with_prefix(manager, prefix),
    )
}

fn enter() -> PersistenceEnvelope {
    PersistenceEnvelope {
        trace_id: "dg-v2".into(),
        circuit: "DowngradeFixture".into(),
        schematic_version: "1".into(),
        event_sequence: 0,
        event_type: PersistenceEventType::Enter,
        step: 0,
        node_id: None,
        outcome_kind: "Enter".into(),
        timestamp_ms: 1,
        payload_hash: None,
        payload: None,
    }
}

#[tokio::test]
#[ignore = "phase 1 of the v0.51.0 downgrade fixture"]
async fn downgrade_seed() {
    let (prefix, postgres, redis) = stores().await;
    postgres.ensure_schema().await.unwrap();
    postgres.append(enter()).await.unwrap();
    redis.ensure_schema().await.unwrap();
    redis.append(enter()).await.unwrap();
    println!("STORE_FORMAT case=G7 phase=seed prefix={prefix} wrote=dg-v2");
}

#[tokio::test]
#[ignore = "phase 3 of the v0.51.0 downgrade fixture"]
async fn downgrade_check() {
    let (prefix, postgres, redis) = stores().await;
    let error = postgres.ensure_schema().await.unwrap_err();
    let found = error.downcast_ref::<StoreFormatError>().cloned();
    assert_eq!(
        found,
        Some(StoreFormatError::LegacyData {
            location: format!("{prefix}_state")
        }),
        "{error:#}"
    );
    redis.ensure_schema().await.unwrap();
    assert!(redis.load("dg-v1").await.unwrap().is_none());
    assert_eq!(redis.load("dg-v2").await.unwrap().unwrap().events.len(), 1);
    println!(
        "STORE_FORMAT case=G7 phase=check postgres=refused:LegacyData redis=started:v1-invisible,v2-intact PASS"
    );
}

/// P2-7 R7: a backup holding version 1 data, restored into an empty server, is
/// refused by `ensure_schema()` and nothing is written. Driven by
/// `scripts/run-legacy-restore-rejection.ps1`, which restores a v0.51.0
/// PostgreSQL dump and the archived v1 Redis RDB.
#[tokio::test]
#[ignore = "requires restored version 1 backups (P2-7 R7)"]
async fn restored_v1_backup_is_refused() {
    use redis::AsyncCommands;
    let pg_prefix = env("RANVIER_LEGACY_POSTGRES_PREFIX");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&env("RANVIER_LEGACY_POSTGRES_URL"))
        .await
        .unwrap();
    let postgres = PostgresPersistenceStore::with_table_prefix(pool.clone(), pg_prefix.clone());
    let error = postgres.ensure_schema().await.unwrap_err();
    assert_eq!(
        error.downcast_ref::<StoreFormatError>().cloned(),
        Some(StoreFormatError::LegacyData {
            location: format!("{pg_prefix}_state")
        }),
        "{error:#}"
    );
    for relation in ["format", "v2_state", "v2_events", "v2_interventions"] {
        let present: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
            .bind(format!("{pg_prefix}_{relation}"))
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(!present, "{pg_prefix}_{relation} must not be created");
    }

    let redis_key = env("RANVIER_LEGACY_REDIS_KEY");
    let client = redis::Client::open(env("RANVIER_LEGACY_REDIS_URL").as_str()).unwrap();
    let mut connection = redis::aio::ConnectionManager::new(client).await.unwrap();
    let before: usize = redis::cmd("DBSIZE")
        .query_async(&mut connection)
        .await
        .unwrap();
    let redis = RedisPersistenceStore::with_prefix(connection.clone(), "ranvier:persistence");
    let error = redis.ensure_schema().await.unwrap_err();
    assert_eq!(
        error.downcast_ref::<StoreFormatError>().cloned(),
        Some(StoreFormatError::LegacyData {
            location: redis_key.clone()
        }),
        "{error:#}"
    );
    let marker: bool = connection
        .exists("ranvier:persistence:format")
        .await
        .unwrap();
    let after: usize = redis::cmd("DBSIZE")
        .query_async(&mut connection)
        .await
        .unwrap();
    assert!(!marker && before == after, "nothing may be written");
    println!(
        "LEGACY_RESTORE postgres=refused:LegacyData redis=refused:LegacyData nothing-written PASS"
    );
}
