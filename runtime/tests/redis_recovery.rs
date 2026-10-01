#![cfg(feature = "persistence-redis")]
use ranvier_runtime::persistence::{
    CompletionState, Intervention, PersistedTrace, PersistenceEnvelope, PersistenceStore,
    RedisPersistenceStore,
};
use std::{fs::OpenOptions, io::Read, path::Path};

#[derive(Debug, PartialEq)]
enum Mode {
    Seed,
    Check,
}

fn mode(value: &str) -> anyhow::Result<Mode> {
    match value {
        "seed" => Ok(Mode::Seed),
        "check" => Ok(Mode::Check),
        _ => anyhow::bail!("explicit seed/check role required"),
    }
}

fn expected(trace: &str) -> PersistedTrace {
    let event = |step| PersistenceEnvelope {
        trace_id: trace.into(),
        circuit: "RedisRecoveryFixture".into(),
        schematic_version: format!("recovery-{step}"),
        step,
        node_id: Some(format!("node-{step}")),
        outcome_kind: "Next".into(),
        timestamp_ms: u64::MAX,
        payload_hash: Some(format!("hash-{step}")),
        payload: Some(serde_json::json!({"step": step, "large": u64::MAX})),
    };
    PersistedTrace {
        trace_id: trace.into(),
        circuit: "RedisRecoveryFixture".into(),
        schematic_version: "recovery-1".into(),
        events: vec![event(0), event(1)],
        interventions: vec![Intervention {
            target_node: "recovery-node".into(),
            payload_override: Some(serde_json::json!({"prior": u64::MAX, "retained": true})),
            timestamp_ms: u64::MAX,
        }],
        resumed_from_step: Some(1),
        completion: Some(CompletionState::Success),
    }
}

fn read_witness(path: &Path) -> anyhow::Result<PersistedTrace> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= 1024 * 1024, "witness exceeds fixture limit");
    Ok(serde_json::from_slice(&bytes)?)
}

// Ignored in ordinary suites: only an explicitly owned rehearsal selects this worker.
#[tokio::test]
#[ignore = "requires explicit recovery role, witness and isolated Redis URL"]
async fn redis_recovery_worker() {
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        let role = mode(&std::env::var("RANVIER_REDIS_RECOVERY_MODE").unwrap()).unwrap();
        let trace = std::env::var("RANVIER_REDIS_RECOVERY_TRACE").unwrap();
        let suffix = trace.strip_prefix("redis-recovery-").unwrap();
        assert_eq!(
            uuid::Uuid::parse_str(suffix).unwrap().simple().to_string(),
            suffix
        );
        let snapshot = std::env::var("RANVIER_REDIS_RECOVERY_SNAPSHOT").unwrap();
        let url = std::env::var("RANVIER_ATOMICITY_REDIS_URL").unwrap();
        assert!(!snapshot.is_empty() && !url.is_empty());
        let contract = expected(&trace);
        // Validate or reserve the witness before any backend writes.
        let output = match role {
            Mode::Seed => Some(
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&snapshot)
                    .unwrap(),
            ),
            Mode::Check => {
                assert_eq!(read_witness(Path::new(&snapshot)).unwrap(), contract);
                None
            }
        };
        let store = RedisPersistenceStore::connect(&url).await.unwrap();
        match role {
            Mode::Seed => {
                assert!(
                    store.load(&trace).await.unwrap().is_none(),
                    "fresh fixture trace required"
                );
                for event in &contract.events {
                    store.append(event.clone()).await.unwrap();
                }
                let cursor = store.resume(&trace, 1).await.unwrap();
                assert_eq!(cursor.trace_id, trace);
                assert_eq!(cursor.next_step, 2);
                store
                    .save_intervention(&trace, contract.interventions[0].clone())
                    .await
                    .unwrap();
                store
                    .complete(&trace, CompletionState::Success)
                    .await
                    .unwrap();
                let actual = store.load(&trace).await.unwrap().unwrap();
                assert_eq!(actual, contract);
                let file = output.unwrap();
                serde_json::to_writer(&file, &actual).unwrap();
                file.sync_all().unwrap();
                println!("REDIS_RECOVERY_SEED acknowledged-full-snapshot synced-witness");
            }
            Mode::Check => {
                assert_eq!(store.load(&trace).await.unwrap(), Some(contract));
                println!("REDIS_RECOVERY_CHECK fresh-process full-snapshot-equality");
            }
        }
        println!("Redis recovery worker pid={}", std::process::id());
    })
    .await
    .expect("bounded recovery worker");
}

#[test]
fn recovery_role_rejects_missing_or_unknown_value() {
    assert_eq!(mode("seed").unwrap(), Mode::Seed);
    assert_eq!(mode("check").unwrap(), Mode::Check);
    for value in ["", "restore", "SEED"] {
        assert!(mode(value).is_err());
    }
}

#[test]
fn existing_witness_cannot_be_overwritten() {
    let path = std::env::temp_dir().join(format!("redis-witness-{}.json", uuid::Uuid::new_v4()));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    serde_json::to_writer(&file, &expected("fixture")).unwrap();
    file.sync_all().unwrap();
    drop(file);
    assert!(
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .is_err()
    );
    assert_eq!(read_witness(&path).unwrap(), expected("fixture"));
    std::fs::remove_file(path).unwrap();
}

#[test]
fn malformed_witness_is_rejected() {
    let path = std::env::temp_dir().join(format!("redis-witness-{}.json", uuid::Uuid::new_v4()));
    std::fs::write(&path, b"{corrupt").unwrap();
    assert!(read_witness(&path).is_err());
    std::fs::remove_file(path).unwrap();
}
