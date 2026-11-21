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
