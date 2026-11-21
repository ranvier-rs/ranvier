//! P2-4 concurrency invariants (D-CONC-DESIGN I1-I6) on every built-in store.
//! InMemory runs by default. PostgreSQL and Redis run with `--ignored` against
//! disposable instances named by `RANVIER_PERSISTENCE_POSTGRES_URL` and
//! `RANVIER_PERSISTENCE_REDIS_URL`.

#[path = "support/store_concurrency.rs"]
mod store_concurrency;

use ranvier_runtime::persistence::*;
use std::sync::Arc;
use store_concurrency::{Connect, Fixture, StoreRef};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn in_memory_store_concurrency_invariants() {
    let store = InMemoryPersistenceStore::new();
    let connect: Connect = Arc::new(move || {
        let store = store.clone();
        Box::pin(async move { Arc::new(store) as StoreRef })
    });
    Fixture::new("in-memory", connect).run_all(20).await;
}

#[cfg(feature = "persistence-postgres")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated PostgreSQL fixture URL"]
async fn postgres_store_concurrency_invariants() {
    let url = std::env::var("RANVIER_PERSISTENCE_POSTGRES_URL")
        .expect("explicit isolated PostgreSQL fixture URL required");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(16)
        .connect(&url)
        .await
        .unwrap();
    let prefix = format!("rsc_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
    PostgresPersistenceStore::with_table_prefix(pool.clone(), prefix.clone())
        .ensure_schema()
        .await
        .unwrap();
    let connect: Connect = Arc::new(move || {
        let store = PostgresPersistenceStore::with_table_prefix(pool.clone(), prefix.clone());
        Box::pin(async move { Arc::new(store) as StoreRef })
    });
    Fixture::new("postgres", connect).run_all(20).await;
}

#[cfg(feature = "persistence-redis")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated Redis fixture URL"]
async fn redis_store_concurrency_invariants() {
    let url = std::env::var("RANVIER_PERSISTENCE_REDIS_URL")
        .expect("explicit isolated Redis fixture URL required");
    // Each call opens an independent connection.
    let connect: Connect = Arc::new(move || {
        let url = url.clone();
        Box::pin(async move {
            Arc::new(RedisPersistenceStore::connect(&url).await.unwrap()) as StoreRef
        })
    });
    Fixture::new("redis", connect).run_all(20).await;
}
