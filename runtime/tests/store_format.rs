//! P2-6 store format marker and version 1 refusal (D-STORE-FORMAT, G1-G8).
//! The decode cases run by default. PostgreSQL and Redis run with `--ignored`
//! against disposable instances named by `RANVIER_PERSISTENCE_POSTGRES_URL`
//! and `RANVIER_PERSISTENCE_REDIS_URL`; every case uses a fresh prefix.

use ranvier_runtime::persistence::*;

#[cfg(any(feature = "persistence-postgres", feature = "persistence-redis"))]
fn format_error(error: &anyhow::Error) -> StoreFormatError {
    error
        .downcast_ref::<StoreFormatError>()
        .cloned()
        .unwrap_or_else(|| panic!("expected StoreFormatError, got {error:#}"))
}

#[cfg(any(feature = "persistence-postgres", feature = "persistence-redis"))]
fn enter(trace_id: &str) -> PersistenceEnvelope {
    PersistenceEnvelope {
        trace_id: trace_id.into(),
        circuit: "StoreFormatFixture".into(),
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

fn trace_json() -> serde_json::Value {
    serde_json::json!({
        "trace_id": "t",
        "circuit": "c",
        "format_version": PERSISTENCE_FORMAT_VERSION,
        "schematic_version": "1",
        "events": [{
            "trace_id": "t",
            "circuit": "c",
            "schematic_version": "1",
            "event_sequence": 0,
            "event_type": "enter",
            "step": 0,
            "node_id": null,
            "outcome_kind": "Enter",
            "timestamp_ms": 1,
            "payload_hash": null,
            "payload": null
        }],
        "interventions": [],
        "resumed_from_step": null,
        "completion": null
    })
}

/// F5: a trace without `format_version`, or an event without `event_type`,
/// no longer decodes as a legacy value.
#[test]
fn traces_without_format_or_event_type_do_not_decode() {
    let complete = trace_json();
    assert!(serde_json::from_value::<PersistedTrace>(complete.clone()).is_ok());

    let mut no_format = complete.clone();
    no_format.as_object_mut().unwrap().remove("format_version");
    assert!(serde_json::from_value::<PersistedTrace>(no_format).is_err());

    let mut no_event_type = complete;
    no_event_type["events"][0]
        .as_object_mut()
        .unwrap()
        .remove("event_type");
    assert!(serde_json::from_value::<PersistedTrace>(no_event_type).is_err());
}

#[test]
fn store_format_errors_name_the_remedy() {
    let legacy = StoreFormatError::LegacyData {
        location: "app_state".into(),
    }
    .to_string();
    assert!(legacy.contains("app_state") && legacy.contains("not supported"));
    let version = StoreFormatError::UnsupportedVersion { found: "3".into() }.to_string();
    assert!(version.contains("\"3\"") && version.contains(&PERSISTENCE_FORMAT_VERSION.to_string()));
    assert!(
        StoreFormatError::NotInitialized
            .to_string()
            .contains("ensure_schema()")
    );
}

#[cfg(feature = "persistence-postgres")]
mod postgres {
    use super::*;
    use sqlx::PgPool;

    fn fresh(pool: &PgPool) -> (String, PostgresPersistenceStore) {
        let prefix = format!("rsf_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
        let store = PostgresPersistenceStore::with_table_prefix(pool.clone(), prefix.clone());
        (prefix, store)
    }

    async fn exists(pool: &PgPool, relation: &str) -> bool {
        sqlx::query_scalar::<_, bool>("SELECT to_regclass($1) IS NOT NULL")
            .bind(relation)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn marker(pool: &PgPool, prefix: &str) -> (i64, Option<String>) {
        sqlx::query_as::<_, (i64, Option<String>)>(&format!(
            "SELECT count(*), max(format_version::text) FROM {prefix}_format"
        ))
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn exec(pool: &PgPool, sql: &str) {
        sqlx::query(sql).execute(pool).await.unwrap();
    }

    fn pass(case: &str) {
        println!("STORE_FORMAT store=postgres case={case} PASS");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires an isolated PostgreSQL fixture URL"]
    async fn postgres_store_format() {
        let url = std::env::var("RANVIER_PERSISTENCE_POSTGRES_URL")
            .expect("explicit isolated PostgreSQL fixture URL required");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(16)
            .connect(&url)
            .await
            .unwrap();

        // G1: repeated and concurrent startup on a new store.
        let (prefix, store) = fresh(&pool);
        store.ensure_schema().await.unwrap();
        store.ensure_schema().await.unwrap();
        assert_eq!(marker(&pool, &prefix).await, (1, Some("2".into())));
        let (prefix, _) = fresh(&pool);
        let mut starts = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let store = PostgresPersistenceStore::with_table_prefix(pool.clone(), prefix.clone());
            starts.spawn(async move { store.ensure_schema().await });
        }
        while let Some(result) = starts.join_next().await {
            result.unwrap().unwrap();
        }
        assert_eq!(marker(&pool, &prefix).await, (1, Some("2".into())));
        // Another instance on the same prefix uses the marker without ensure_schema.
        let other = PostgresPersistenceStore::with_table_prefix(pool.clone(), prefix.clone());
        other.append(enter("g1")).await.unwrap();
        assert!(other.load("g1").await.unwrap().is_some());
        pass("G1");

        // G2: version 1 data refuses startup and writes nothing.
        let (prefix, store) = fresh(&pool);
        exec(
            &pool,
            &format!("CREATE TABLE {prefix}_state (trace_id TEXT)"),
        )
        .await;
        exec(&pool, &format!("INSERT INTO {prefix}_state VALUES ('old')")).await;
        let error = store.ensure_schema().await.unwrap_err();
        assert_eq!(
            format_error(&error),
            StoreFormatError::LegacyData {
                location: format!("{prefix}_state")
            }
        );
        let lazy = PostgresPersistenceStore::with_table_prefix(pool.clone(), prefix.clone());
        let error = lazy.append(enter("g2")).await.unwrap_err();
        assert!(matches!(
            format_error(&error),
            StoreFormatError::LegacyData { .. }
        ));
        assert!(!exists(&pool, &format!("{prefix}_format")).await);
        assert!(!exists(&pool, &format!("{prefix}_v2_state")).await);
        assert!(!exists(&pool, &format!("{prefix}_v2_events")).await);
        // Empty version 1 tables hold no data and are accepted.
        let (prefix, store) = fresh(&pool);
        exec(
            &pool,
            &format!("CREATE TABLE {prefix}_events (trace_id TEXT)"),
        )
        .await;
        store.ensure_schema().await.unwrap();
        assert_eq!(marker(&pool, &prefix).await, (1, Some("2".into())));
        // Version 1 rows written after the marker (a downgraded v0.51.x) are
        // still refused: the version 1 tables are checked on every start.
        exec(
            &pool,
            &format!("INSERT INTO {prefix}_events VALUES ('late')"),
        )
        .await;
        let reopened = PostgresPersistenceStore::with_table_prefix(pool.clone(), prefix.clone());
        let error = reopened.append(enter("g2-late")).await.unwrap_err();
        assert_eq!(
            format_error(&error),
            StoreFormatError::LegacyData {
                location: format!("{prefix}_events")
            }
        );
        assert!(reopened.ensure_schema().await.is_err());
        pass("G2");

        // G3: a later format marker refuses startup and operations.
        let (prefix, store) = fresh(&pool);
        exec(
            &pool,
            &format!(
                "CREATE TABLE {prefix}_format (id SMALLINT PRIMARY KEY, format_version INTEGER NOT NULL)"
            ),
        )
        .await;
        exec(&pool, &format!("INSERT INTO {prefix}_format VALUES (1, 3)")).await;
        let error = store.ensure_schema().await.unwrap_err();
        assert_eq!(
            format_error(&error),
            StoreFormatError::UnsupportedVersion { found: "3".into() }
        );
        assert!(!exists(&pool, &format!("{prefix}_v2_state")).await);
        // A store this build initialized and a later build re-marked.
        let (prefix, store) = fresh(&pool);
        store.ensure_schema().await.unwrap();
        store.append(enter("g3")).await.unwrap();
        exec(
            &pool,
            &format!("UPDATE {prefix}_format SET format_version = 3"),
        )
        .await;
        let later = PostgresPersistenceStore::with_table_prefix(pool.clone(), prefix.clone());
        for error in [
            later.append(enter("g3-new")).await.unwrap_err(),
            later.load("g3").await.unwrap_err(),
            later.ensure_schema().await.unwrap_err(),
        ] {
            assert_eq!(
                format_error(&error),
                StoreFormatError::UnsupportedVersion { found: "3".into() }
            );
        }
        let events: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {prefix}_v2_events"))
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(events, 1);
        pass("G3");

        // G4: a marker that is not a number.
        let (prefix, store) = fresh(&pool);
        exec(
            &pool,
            &format!("CREATE TABLE {prefix}_format (id SMALLINT PRIMARY KEY, format_version TEXT)"),
        )
        .await;
        exec(
            &pool,
            &format!("INSERT INTO {prefix}_format VALUES (1, 'abc')"),
        )
        .await;
        let error = store.ensure_schema().await.unwrap_err();
        assert_eq!(
            format_error(&error),
            StoreFormatError::UnsupportedVersion {
                found: "abc".into()
            }
        );
        pass("G4");

        // G5: the first operation without ensure_schema.
        let (prefix, store) = fresh(&pool);
        let error = store.append(enter("g5")).await.unwrap_err();
        assert_eq!(format_error(&error), StoreFormatError::NotInitialized);
        assert!(!exists(&pool, &format!("{prefix}_format")).await);
        pass("G5");

        // G6: version 2 data without a marker (builds before P2-6).
        let (prefix, store) = fresh(&pool);
        store.ensure_schema().await.unwrap();
        store.append(enter("g6")).await.unwrap();
        exec(&pool, &format!("DROP TABLE {prefix}_format")).await;
        let reopened = PostgresPersistenceStore::with_table_prefix(pool.clone(), prefix.clone());
        reopened.ensure_schema().await.unwrap();
        assert_eq!(marker(&pool, &prefix).await, (1, Some("2".into())));
        assert_eq!(reopened.load("g6").await.unwrap().unwrap().events.len(), 1);
        pass("G6");
    }
}

#[cfg(feature = "persistence-redis")]
mod redis_store {
    use super::*;
    use ranvier_core::prelude::*;
    use ranvier_runtime::{Axon, EffectBoundary, ExecutionReport, PersistenceOperation};
    use redis::AsyncCommands;
    use redis::aio::ConnectionManager;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    fn fresh_prefix() -> String {
        format!("rsf:{}", &uuid::Uuid::new_v4().simple().to_string()[..12])
    }

    async fn get(manager: &ConnectionManager, key: &str) -> Option<String> {
        manager.clone().get(key).await.unwrap()
    }

    async fn set(manager: &ConnectionManager, key: &str, value: &str) {
        let _: () = manager.clone().set(key, value).await.unwrap();
    }

    fn pass(case: &str) {
        println!("STORE_FORMAT store=redis case={case} PASS");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires an isolated Redis fixture URL"]
    async fn redis_store_format() {
        let url = std::env::var("RANVIER_PERSISTENCE_REDIS_URL")
            .expect("explicit isolated Redis fixture URL required");
        let manager = ConnectionManager::new(redis::Client::open(url.as_str()).unwrap())
            .await
            .unwrap();
        let store = |prefix: &str| RedisPersistenceStore::with_prefix(manager.clone(), prefix);

        // G1: repeated and concurrent startup on a new store.
        let prefix = fresh_prefix();
        store(&prefix).ensure_schema().await.unwrap();
        store(&prefix).ensure_schema().await.unwrap();
        assert_eq!(
            get(&manager, &format!("{prefix}:format")).await,
            Some("2".into())
        );
        let prefix = fresh_prefix();
        let mut starts = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let store = store(&prefix);
            starts.spawn(async move { store.ensure_schema().await });
        }
        while let Some(result) = starts.join_next().await {
            result.unwrap().unwrap();
        }
        assert_eq!(
            get(&manager, &format!("{prefix}:format")).await,
            Some("2".into())
        );
        // Glob characters in the prefix are matched literally.
        let prefix = format!("{}:a*b", fresh_prefix());
        set(
            &manager,
            &format!("{}:aXb:t", &prefix[..prefix.len() - 4]),
            "{}",
        )
        .await;
        store(&prefix).ensure_schema().await.unwrap();
        pass("G1");

        // G2: version 1 keys refuse startup and write nothing.
        let prefix = fresh_prefix();
        set(&manager, &format!("{prefix}:old-trace"), "{}").await;
        let error = store(&prefix).ensure_schema().await.unwrap_err();
        assert_eq!(
            format_error(&error),
            StoreFormatError::LegacyData {
                location: format!("{prefix}:old-trace")
            }
        );
        let error = store(&prefix).append(enter("g2")).await.unwrap_err();
        assert!(matches!(
            format_error(&error),
            StoreFormatError::LegacyData { .. }
        ));
        assert_eq!(get(&manager, &format!("{prefix}:format")).await, None);
        assert_eq!(get(&manager, &format!("{prefix}:v2:g2")).await, None);
        // Once the marker exists the keyspace is not scanned again, so a
        // version 1 key written later is not detected (documented limit).
        let prefix = fresh_prefix();
        store(&prefix).ensure_schema().await.unwrap();
        set(&manager, &format!("{prefix}:late-trace"), "{}").await;
        store(&prefix).ensure_schema().await.unwrap();
        assert!(store(&prefix).load("late-trace").await.unwrap().is_none());
        pass("G2");

        // G3: a later format marker refuses startup and operations.
        let prefix = fresh_prefix();
        let initialized = store(&prefix);
        initialized.ensure_schema().await.unwrap();
        initialized.append(enter("g3")).await.unwrap();
        set(&manager, &format!("{prefix}:format"), "3").await;
        let later = store(&prefix);
        for error in [
            later.append(enter("g3-new")).await.unwrap_err(),
            later.load("g3").await.unwrap_err(),
            later.ensure_schema().await.unwrap_err(),
        ] {
            assert_eq!(
                format_error(&error),
                StoreFormatError::UnsupportedVersion { found: "3".into() }
            );
        }
        assert_eq!(get(&manager, &format!("{prefix}:v2:g3-new")).await, None);
        pass("G3");

        // G4: a marker that is not a number.
        let prefix = fresh_prefix();
        set(&manager, &format!("{prefix}:format"), "abc").await;
        let error = store(&prefix).ensure_schema().await.unwrap_err();
        assert_eq!(
            format_error(&error),
            StoreFormatError::UnsupportedVersion {
                found: "abc".into()
            }
        );
        pass("G4");

        // G5: the first operation without ensure_schema records the marker.
        let prefix = fresh_prefix();
        store(&prefix).append(enter("g5")).await.unwrap();
        assert_eq!(
            get(&manager, &format!("{prefix}:format")).await,
            Some("2".into())
        );
        pass("G5");

        // G6: version 2 data without a marker (builds before P2-6).
        let prefix = fresh_prefix();
        let initialized = store(&prefix);
        initialized.ensure_schema().await.unwrap();
        initialized.append(enter("g6")).await.unwrap();
        let _: () = manager
            .clone()
            .del(format!("{prefix}:format"))
            .await
            .unwrap();
        let reopened = store(&prefix);
        reopened.ensure_schema().await.unwrap();
        assert_eq!(
            get(&manager, &format!("{prefix}:format")).await,
            Some("2".into())
        );
        assert_eq!(reopened.load("g6").await.unwrap().unwrap().events.len(), 1);
        pass("G6");

        // G8: a version 2 key holding JSON without the format fields.
        let prefix = fresh_prefix();
        store(&prefix).ensure_schema().await.unwrap();
        let mut value = trace_json();
        value.as_object_mut().unwrap().remove("format_version");
        value["events"][0]
            .as_object_mut()
            .unwrap()
            .remove("event_type");
        value["trace_id"] = "g8".into();
        value["circuit"] = "StoreFormatFixture".into();
        value["events"][0]["trace_id"] = "g8".into();
        value["events"][0]["circuit"] = "StoreFormatFixture".into();
        let stored = value.to_string();
        set(&manager, &format!("{prefix}:v2:g8"), &stored).await;
        let effects = Arc::new(AtomicUsize::new(0));
        let axon = Axon::<u32, u32, String, ()>::new("StoreFormatFixture")
            .then(CountEffect(effects.clone()));
        let mut bus = Bus::new();
        bus.insert(PersistenceHandle::from_store(store(&prefix)));
        bus.insert(PersistenceTraceId::new("g8"));
        let report = axon.execute(1, &(), &mut bus).await;
        assert!(
            matches!(
                &report,
                ExecutionReport::BlockedBeforeEffect {
                    operation: PersistenceOperation::Load,
                    effect: EffectBoundary::NotInvoked,
                    ..
                }
            ),
            "{report:?}"
        );
        assert_eq!(effects.load(Ordering::SeqCst), 0);
        assert_eq!(
            get(&manager, &format!("{prefix}:v2:g8")).await,
            Some(stored)
        );
        pass("G8");
    }
}
