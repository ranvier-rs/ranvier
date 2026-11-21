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
    let node = |step: u64| format!("node-{step}");
    let event = |event_sequence: u64,
                 step: u64,
                 event_type: ranvier_runtime::persistence::PersistenceEventType,
                 node_id: Option<String>,
                 outcome_kind: &str,
                 payload: Option<serde_json::Value>| PersistenceEnvelope {
        trace_id: trace.into(),
        circuit: "RedisRecoveryFixture".into(),
        schematic_version: format!("recovery-{step}"),
        event_sequence,
        event_type,
        step,
        node_id,
        outcome_kind: outcome_kind.into(),
        timestamp_ms: u64::MAX,
        payload_hash: Some(format!("hash-{event_sequence}")),
        payload,
    };
    PersistedTrace {
        trace_id: trace.into(),
        circuit: "RedisRecoveryFixture".into(),
        format_version: ranvier_runtime::PERSISTENCE_FORMAT_VERSION,
        schematic_version: "recovery-2".into(),
        events: vec![
            event(
                0,
                0,
                ranvier_runtime::PersistenceEventType::Enter,
                None,
                "Enter",
                None,
            ),
            event(
                1,
                0,
                ranvier_runtime::PersistenceEventType::Intent,
                Some(node(0)),
                "Intent",
                Some(serde_json::json!({"input": 0, "large": u64::MAX})),
            ),
            event(
                2,
                0,
                ranvier_runtime::PersistenceEventType::Result,
                Some(node(0)),
                "Next",
                Some(serde_json::json!({"step": 0, "large": u64::MAX})),
            ),
            event(
                3,
                1,
                ranvier_runtime::PersistenceEventType::Intent,
                Some(node(1)),
                "Intent",
                Some(serde_json::json!({"input": 1, "large": u64::MAX})),
            ),
            event(
                4,
                1,
                ranvier_runtime::PersistenceEventType::Result,
                Some(node(1)),
                "Next",
                Some(serde_json::json!({"step": 1, "large": u64::MAX})),
            ),
            event(
                5,
                2,
                ranvier_runtime::PersistenceEventType::Terminal,
                None,
                "Success",
                Some(serde_json::json!({"status": "complete"})),
            ),
        ],
        interventions: vec![Intervention {
            target_node: "recovery-node".into(),
            payload_override: Some(serde_json::json!({"prior": u64::MAX, "retained": true})),
            timestamp_ms: u64::MAX,
            expected_event_sequence: 5,
        }],
        resumed_from_step: Some(1),
        completion: Some(CompletionState::Success),
    }
}

fn read_witness(path: &Path) -> anyhow::Result<PersistedTrace> {
    let metadata = std::fs::symlink_metadata(path)?;
    anyhow::ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "regular witness file required"
    );
    anyhow::ensure!(
        metadata.len() <= 1024 * 1024,
        "witness exceeds fixture limit"
    );
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
                // The intervention is bound to the revision before the terminal event.
                let (terminal, journal) = contract.events.split_last().unwrap();
                for event in journal {
                    store.append(event.clone()).await.unwrap();
                }
                let cursor = store.resume(&trace, 1).await.unwrap();
                assert_eq!(cursor.trace_id, trace);
                assert_eq!(cursor.next_step, 2);
                store
                    .save_intervention(&trace, contract.interventions[0].clone())
                    .await
                    .unwrap();
                store.append(terminal.clone()).await.unwrap();
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

#[test]
fn oversized_witness_is_rejected() {
    use std::io::Write;
    let path = std::env::temp_dir().join(format!("redis-witness-{}.json", uuid::Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    file.write_all(&vec![b' '; 1024 * 1024 + 1]).unwrap();
    drop(file);
    assert!(read_witness(&path).is_err());
    std::fs::remove_file(path).unwrap();
}

#[cfg(unix)]
#[test]
fn symlink_witness_is_rejected_without_changing_target() {
    let path = std::env::temp_dir().join(format!("redis-witness-{}.json", uuid::Uuid::new_v4()));
    let link = path.with_extension("link");
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    serde_json::to_writer(&file, &expected("fixture")).unwrap();
    drop(file);
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(read_witness(&link).is_err());
    assert_eq!(read_witness(&path).unwrap(), expected("fixture"));
    std::fs::remove_file(link).unwrap();
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
#[ignore = "requires explicitly isolated Redis fixture URL"]
async fn redis_legacy_null_blocks_execution_and_pre_cancelled_cleanup() {
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

        let url = std::env::var("RANVIER_ATOMICITY_REDIS_URL")
            .expect("explicit isolated database required");
        let store = RedisPersistenceStore::connect(&url).await.unwrap();
        let prefix = uuid::Uuid::new_v4().simple().to_string();
        use redis::AsyncCommands;
        let mut raw =
            redis::aio::ConnectionManager::new(redis::Client::open(url.as_str()).unwrap())
                .await
                .unwrap();
        for mode in [0, 1, 2] {
            let trace_id = format!("null-{prefix}-mode-{mode}");
            let effects = Arc::new(AtomicUsize::new(0));
            let cleanup = Arc::new(AtomicUsize::new(0));
            let axon = Axon::<u32, u32, String>::new("AtomicFixture").then(Count(effects.clone()));
            let seed = PersistenceEnvelope {
                trace_id: trace_id.clone(),
                circuit: axon.schematic().name.clone(),
                schematic_version: axon.schematic().schema_version.clone(),
                event_sequence: 0,
                event_type: ranvier_runtime::persistence::PersistenceEventType::Enter,
                step: 0,
                node_id: axon.schematic().nodes.first().map(|node| node.id.clone()),
                outcome_kind: "Enter".into(),
                timestamp_ms: 0,
                payload_hash: None,
                payload: None,
            };
            store.append(seed).await.unwrap();
            store
                .append(PersistenceEnvelope {
                    trace_id: trace_id.clone(),
                    circuit: axon.schematic().name.clone(),
                    schematic_version: axon.schematic().schema_version.clone(),
                    event_sequence: 1,
                    event_type: ranvier_runtime::persistence::PersistenceEventType::Intent,
                    step: 1,
                    node_id: Some(axon.schematic().nodes[1].id.clone()),
                    outcome_kind: "Intent".into(),
                    timestamp_ms: 0,
                    payload_hash: None,
                    payload: Some(serde_json::json!(7)),
                })
                .await
                .unwrap();
            store
                .append(PersistenceEnvelope {
                    trace_id: trace_id.clone(),
                    circuit: axon.schematic().name.clone(),
                    schematic_version: axon.schematic().schema_version.clone(),
                    event_sequence: 2,
                    event_type: ranvier_runtime::persistence::PersistenceEventType::Result,
                    step: 1,
                    node_id: Some(axon.schematic().nodes[1].id.clone()),
                    outcome_kind: "Next".into(),
                    timestamp_ms: 0,
                    payload_hash: None,
                    payload: Some(serde_json::Value::Null),
                })
                .await
                .unwrap();
            let before = store.load(&trace_id).await.unwrap().unwrap();
            assert_eq!(before.events[2].payload, Some(serde_json::Value::Null));
            let key = format!("ranvier:persistence:v2:{trace_id}");
            let before_wire: String = raw.get(&key).await.unwrap();
            let wire: serde_json::Value = serde_json::from_str(&before_wire).unwrap();
            assert_eq!(
                wire["events"][2].get("payload"),
                Some(&serde_json::Value::Null)
            );
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
            let after_wire: String = raw.get(&key).await.unwrap();
            assert_eq!(after_wire, before_wire);
        }
        println!("REDIS_LEGACY_NULL modes=3 effects=0 cleanup=0 full-traces-unchanged");
    })
    .await
    .expect("isolated recovery fixture must finish within 15 seconds");
}
