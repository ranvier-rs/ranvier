#![cfg(all(feature = "persistence-postgres", feature = "persistence-redis"))]

use ranvier_runtime::persistence::{
    CompletionState, Intervention, PersistedTrace, PersistenceEnvelope, PersistenceStore,
    PostgresPersistenceStore, RedisPersistenceStore,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Seed,
    Check,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    Postgres,
    Redis,
}

fn parse_role(value: &str) -> anyhow::Result<Role> {
    match value {
        "seed" => Ok(Role::Seed),
        "check" => Ok(Role::Check),
        _ => anyhow::bail!("explicit seed/check role required"),
    }
}

fn parse_backend(value: &str) -> anyhow::Result<Backend> {
    match value {
        "postgres" => Ok(Backend::Postgres),
        "redis" => Ok(Backend::Redis),
        _ => anyhow::bail!("explicit postgres/redis backend required"),
    }
}

fn parse_postgres_prefix(value: &str) -> anyhow::Result<&str> {
    match value {
        "compat_old" | "compat_current" => Ok(value),
        _ => anyhow::bail!("explicit fixture prefix required"),
    }
}

fn expected_trace(trace_id: &str, backend: Backend) -> PersistedTrace {
    let node_id = (backend == Backend::Redis).then(|| "synthetic-node".to_string());
    let events = [1_u64, 2]
        .into_iter()
        .map(|step| PersistenceEnvelope {
            trace_id: trace_id.to_string(),
            circuit: "StorageCompatibilityFixture".to_string(),
            schematic_version: format!("compat-{step}"),
            step,
            node_id: node_id.clone(),
            outcome_kind: "Next".to_string(),
            timestamp_ms: 1_700_000_000_000 + step,
            payload_hash: Some(format!("synthetic-hash-{step}")),
            payload: Some(serde_json::json!({ "step": step, "synthetic": true })),
        })
        .collect();

    PersistedTrace {
        trace_id: trace_id.to_string(),
        circuit: "StorageCompatibilityFixture".to_string(),
        schematic_version: "compat-2".to_string(),
        events,
        interventions: vec![Intervention {
            target_node: "synthetic-node".to_string(),
            payload_override: Some(serde_json::json!({ "approved": true, "source": "fixture" })),
            timestamp_ms: 1_700_000_000_100,
        }],
        resumed_from_step: Some(2),
        completion: Some(CompletionState::Success),
    }
}

async fn exercise<S: PersistenceStore>(
    store: &S,
    role: Role,
    expected: PersistedTrace,
) -> anyhow::Result<()> {
    let trace_id = expected.trace_id.clone();
    match role {
        Role::Seed => {
            anyhow::ensure!(
                store.load(&trace_id).await?.is_none(),
                "fixture trace already exists before seeding"
            );
            for event in &expected.events {
                store.append(event.clone()).await.map_err(|error| {
                    anyhow::anyhow!("append step {} failed: {error:#}", event.step)
                })?;
            }
            let cursor = store
                .resume(&trace_id, 2)
                .await
                .map_err(|error| anyhow::anyhow!("resume failed: {error:#}"))?;
            anyhow::ensure!(cursor.trace_id == trace_id, "resume trace ID mismatch");
            anyhow::ensure!(cursor.next_step == 3, "resume next step mismatch");
            store
                .save_intervention(&trace_id, expected.interventions[0].clone())
                .await
                .map_err(|error| anyhow::anyhow!("save intervention failed: {error:#}"))?;
            store
                .complete(&trace_id, CompletionState::Success)
                .await
                .map_err(|error| anyhow::anyhow!("complete failed: {error:#}"))?;
            let actual = store
                .load(&trace_id)
                .await
                .map_err(|error| anyhow::anyhow!("read after seed failed: {error:#}"))?;
            anyhow::ensure!(actual == Some(expected), "seed snapshot mismatch");
        }
        Role::Check => {
            let actual = store
                .load(&trace_id)
                .await
                .map_err(|error| anyhow::anyhow!("cross-version read failed: {error:#}"))?;
            anyhow::ensure!(actual == Some(expected), "cross-version snapshot mismatch");
        }
    }
    Ok(())
}

// This worker is invoked only by the owned compatibility rehearsal. It records
// observations for both adapters; it does not select a supported schema floor.
#[tokio::test]
#[ignore = "requires explicitly isolated PostgreSQL and Redis fixture services"]
async fn storage_compatibility_worker() {
    let role = parse_role(&std::env::var("RANVIER_STORAGE_COMPAT_ROLE").unwrap()).unwrap();
    let backend = parse_backend(&std::env::var("RANVIER_STORAGE_COMPAT_BACKEND").unwrap()).unwrap();
    let trace_id = std::env::var("RANVIER_STORAGE_COMPAT_TRACE").unwrap();
    let suffix = trace_id.strip_prefix("storage-compat-").unwrap();
    assert_eq!(
        uuid::Uuid::parse_str(suffix).unwrap().simple().to_string(),
        suffix
    );
    let expected = expected_trace(&trace_id, backend);

    let result = tokio::time::timeout(std::time::Duration::from_secs(90), async {
        match backend {
            Backend::Postgres => {
                let url = std::env::var("RANVIER_STORAGE_COMPAT_POSTGRES_URL").unwrap();
                let prefix = std::env::var("RANVIER_STORAGE_COMPAT_POSTGRES_PREFIX").unwrap();
                let prefix = parse_postgres_prefix(&prefix).unwrap();
                let pool = sqlx::postgres::PgPoolOptions::new()
                    .max_connections(2)
                    .connect(&url)
                    .await
                    .map_err(|error| anyhow::anyhow!("connect failed: {error:#}"))?;
                let store = PostgresPersistenceStore::with_table_prefix(pool, prefix);
                store
                    .ensure_schema()
                    .await
                    .map_err(|error| anyhow::anyhow!("ensure schema failed: {error:#}"))?;
                exercise(&store, role, expected).await
            }
            Backend::Redis => {
                let url = std::env::var("RANVIER_STORAGE_COMPAT_REDIS_URL").unwrap();
                let store = RedisPersistenceStore::connect(&url)
                    .await
                    .map_err(|error| anyhow::anyhow!("connect failed: {error:#}"))?;
                exercise(&store, role, expected).await
            }
        }
    })
    .await;
    let outcome = match result {
        Ok(outcome) => outcome,
        Err(_) => Err(anyhow::anyhow!("worker deadline expired")),
    };

    match outcome {
        Ok(()) => println!(
            "STORAGE_COMPATIBILITY backend={} role={} result=PASS detail=full-snapshot-equality",
            match backend {
                Backend::Postgres => "postgres",
                Backend::Redis => "redis",
            },
            match role {
                Role::Seed => "seed",
                Role::Check => "check",
            }
        ),
        Err(error) => println!(
            "STORAGE_COMPATIBILITY backend={} role={} result=FAIL detail={}",
            match backend {
                Backend::Postgres => "postgres",
                Backend::Redis => "redis",
            },
            match role {
                Role::Seed => "seed",
                Role::Check => "check",
            },
            error.to_string().replace('\n', " ").replace('\r', " ")
        ),
    }
}

#[test]
fn compatibility_worker_requires_explicit_role_and_backend() {
    assert_eq!(parse_role("seed").unwrap(), Role::Seed);
    assert_eq!(parse_role("check").unwrap(), Role::Check);
    assert!(parse_role("").is_err());
    assert!(parse_role("restore").is_err());

    assert_eq!(parse_backend("postgres").unwrap(), Backend::Postgres);
    assert_eq!(parse_backend("redis").unwrap(), Backend::Redis);
    assert!(parse_backend("").is_err());
    assert!(parse_backend("all").is_err());

    assert_eq!(parse_postgres_prefix("compat_old").unwrap(), "compat_old");
    assert_eq!(
        parse_postgres_prefix("compat_current").unwrap(),
        "compat_current"
    );
    assert!(parse_postgres_prefix("compat; DROP TABLE fixture").is_err());
}
