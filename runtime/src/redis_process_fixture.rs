//! Test-only process fixture; control traffic never enters the persistence wire API.
use super::*;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::{Duration, sleep, timeout},
};

/// A v2 checkpoint. Competing writers use the same `sequence` with a
/// different `step`, so exactly one of them can be acknowledged.
fn event(trace_id: &str, sequence: u64, step: u64) -> PersistenceEnvelope {
    PersistenceEnvelope {
        trace_id: trace_id.into(),
        circuit: "ProcessFixture".into(),
        schematic_version: format!("process-version-{step}"),
        event_sequence: sequence,
        event_type: crate::PersistenceEventType::Checkpoint,
        step,
        node_id: Some(format!("node-{step}")),
        outcome_kind: "Checkpoint".into(),
        timestamp_ms: u64::MAX,
        payload_hash: None,
        payload: Some(json!({"step": step, "large": u64::MAX})),
    }
}

fn terminal(trace_id: &str, sequence: u64) -> PersistenceEnvelope {
    PersistenceEnvelope {
        trace_id: trace_id.into(),
        circuit: "ProcessFixture".into(),
        schematic_version: "process-terminal".into(),
        event_sequence: sequence,
        event_type: crate::PersistenceEventType::Terminal,
        step: sequence,
        node_id: None,
        outcome_kind: "Success".into(),
        timestamp_ms: u64::MAX,
        payload_hash: None,
        payload: None,
    }
}

async fn send(stream: &mut TcpStream, value: Value) {
    let bytes = serde_json::to_vec(&value).unwrap();
    assert!(bytes.len() <= 1024 * 1024);
    stream.write_u32(bytes.len() as u32).await.unwrap();
    stream.write_all(&bytes).await.unwrap();
}

async fn receive(stream: &mut TcpStream) -> Value {
    let length = stream.read_u32().await.unwrap() as usize;
    assert!(length > 0 && length <= 1024 * 1024, "bounded fixture frame");
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

// Selected explicitly by the parent; excluded from the CI redis_ name filter.
#[tokio::test]
#[ignore = "internal worker requires parent-owned fixture inputs"]
async fn isolated_process_worker() {
    timeout(Duration::from_secs(15), async {
        let url = std::env::var("RANVIER_ATOMICITY_REDIS_URL").unwrap();
        let trace = std::env::var("RANVIER_PROCESS_TRACE").unwrap();
        let address = std::env::var("RANVIER_PROCESS_ADDRESS").unwrap();
        let token = std::env::var("RANVIER_PROCESS_TOKEN").unwrap();
        let step: u64 = std::env::var("RANVIER_PROCESS_STEP")
            .unwrap()
            .parse()
            .unwrap();
        assert!(matches!(step, 1 | 2));
        assert!(!trace.is_empty() && !token.is_empty());
        let mut store = RedisPersistenceStore::connect(&url).await.unwrap();
        let gate = Arc::new(RedisWriteGate {
            ready: tokio::sync::Semaphore::new(0),
            release: tokio::sync::Semaphore::new(0),
            expected: tokio::sync::Mutex::new(None),
        });
        store.write_gate = Some(gate.clone());
        let mut channel = TcpStream::connect(address).await.unwrap();
        let writer = tokio::spawn(async move { store.append(event(&trace, 1, step)).await });
        gate.ready.acquire().await.unwrap().forget();
        send(
            &mut channel,
            json!({
                "pid": std::process::id(), "step": step, "token": token,
                "raw": gate.expected.lock().await.clone(),
            }),
        )
        .await;
        assert_eq!(channel.read_u8().await.unwrap(), 1);
        gate.release.add_permits(1);
        let outcome = match writer.await.unwrap() {
            Ok(()) => "ack",
            Err(error)
                if matches!(
                    error.downcast_ref::<PersistenceConflict>().map(|c| &c.kind),
                    Some(PersistenceConflictKind::ConcurrentUpdate)
                ) =>
            {
                "conflict"
            }
            Err(error) => panic!("unexpected persistence outcome: {error}"),
        };
        send(
            &mut channel,
            json!({"pid": std::process::id(), "step": step, "outcome": outcome}),
        )
        .await;
    })
    .await
    .expect("bounded isolated worker");
}

struct OwnedProcess(std::process::Child);

impl OwnedProcess {
    fn start(trace: &str, step: u64, address: std::net::SocketAddr, token: &str) -> Self {
        Self(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "persistence::process_fixture::isolated_process_worker",
                    "--ignored",
                    "--nocapture",
                ])
                .env("RANVIER_PROCESS_TRACE", trace)
                .env("RANVIER_PROCESS_STEP", step.to_string())
                .env("RANVIER_PROCESS_ADDRESS", address.to_string())
                .env("RANVIER_PROCESS_TOKEN", token)
                .spawn()
                .unwrap(),
        )
    }

    async fn finish(&mut self) {
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                assert!(status.success(), "fixture worker must exit successfully");
                return;
            }
            sleep(Duration::from_millis(10)).await;
        }
    }

    #[cfg(unix)]
    async fn kill_and_reap(&mut self) -> std::process::ExitStatus {
        assert!(
            self.0.try_wait().unwrap().is_none(),
            "owned worker still live"
        );
        self.0.kill().unwrap();
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                // try_wait reaps the child and retains its terminal status.
                return status;
            }
            sleep(Duration::from_millis(10)).await;
        }
    }
}

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

#[tokio::test]
#[ignore = "requires explicitly isolated Redis fixture URL"]
async fn redis_independent_process_appends_preserve_acknowledged_write() {
    let url = std::env::var("RANVIER_ATOMICITY_REDIS_URL").unwrap();
    let observer = timeout(Duration::from_secs(5), RedisPersistenceStore::connect(&url))
        .await
        .unwrap()
        .unwrap();
    for first_step in [1, 2] {
        let trace = format!("os-process-{}", uuid::Uuid::new_v4().simple());
        observer.append(event(&trace, 0, 0)).await.unwrap();
        observer.resume(&trace, 0).await.unwrap();
        observer
            .save_intervention(
                &trace,
                Intervention {
                    target_node: "prior-node".into(),
                    payload_override: Some(json!({"prior": u64::MAX})),
                    timestamp_ms: u64::MAX,
                    expected_event_sequence: 1,
                },
            )
            .await
            .unwrap();
        let mut expected = observer.load(&trace).await.unwrap().unwrap();
        let original = observer.read_trace(&trace).await.unwrap().0.unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let token = uuid::Uuid::new_v4().to_string();
        let mut first = OwnedProcess::start(&trace, 1, address, &token);
        let mut second = OwnedProcess::start(&trace, 2, address, &token);
        let pid1 = first.0.id();
        let pid2 = second.0.id();
        assert_ne!(pid1, pid2);
        assert_ne!(pid1, std::process::id());
        assert_ne!(pid2, std::process::id());
        timeout(Duration::from_secs(20), async {
            let mut channels = Vec::new();
            for _ in 0..2 {
                let (mut channel, _) = listener.accept().await.unwrap();
                let ready = receive(&mut channel).await;
                let step = ready["step"].as_u64().unwrap();
                assert!(matches!(step, 1 | 2));
                assert_eq!(ready["pid"], if step == 1 { pid1 } else { pid2 });
                assert_eq!(ready["token"], token);
                assert_eq!(ready["raw"], original);
                assert!(channels.iter().all(|(prior, _)| *prior != step));
                channels.push((step, channel));
            }
            channels.sort_by_key(|(step, _)| if *step == first_step { 0 } else { 1 });
            for (index, (step, channel)) in channels.iter_mut().enumerate() {
                channel.write_u8(1).await.unwrap();
                let outcome = receive(channel).await;
                assert_eq!(outcome["step"], *step);
                assert_eq!(outcome["pid"], if *step == 1 { pid1 } else { pid2 });
                assert_eq!(
                    outcome["outcome"],
                    if index == 0 { "ack" } else { "conflict" }
                );
                if index == 0 {
                    let accepted = event(&trace, 1, *step);
                    expected.schematic_version = accepted.schematic_version.clone();
                    expected.events.push(accepted);
                }
                assert_eq!(observer.load(&trace).await.unwrap().unwrap(), expected);
            }
            first.finish().await;
            second.finish().await;
            observer.append(terminal(&trace, 2)).await.unwrap();
            expected.schematic_version = "process-terminal".into();
            expected.events.push(terminal(&trace, 2));
            observer
                .complete(&trace, CompletionState::Success)
                .await
                .unwrap();
            expected.completion = Some(CompletionState::Success);
            assert!(
                observer
                    .append(event(&trace, 3, 3))
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("already completed")
            );
            assert_eq!(observer.load(&trace).await.unwrap().unwrap(), expected);
        })
        .await
        .expect("bounded independent-process fixture");
        println!(
            "Redis process identities: parent={} writer1={pid1} writer2={pid2}",
            std::process::id()
        );
        println!(
            "REDIS_PROCESS_PASS first_step={first_step} independent-pids same-read exact-acknowledged-snapshot"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires explicitly isolated Redis fixture URL"]
async fn redis_killed_pre_eval_process_preserves_seed_and_allows_fresh_writer() {
    use std::os::unix::process::ExitStatusExt;

    let url = std::env::var("RANVIER_ATOMICITY_REDIS_URL").unwrap();
    let observer = timeout(Duration::from_secs(5), RedisPersistenceStore::connect(&url))
        .await
        .unwrap()
        .unwrap();
    let trace = format!("killed-process-{}", uuid::Uuid::new_v4().simple());
    observer.append(event(&trace, 0, 0)).await.unwrap();
    observer.resume(&trace, 0).await.unwrap();
    observer
        .save_intervention(
            &trace,
            Intervention {
                target_node: "kill-seed-node".into(),
                payload_override: Some(json!({"prior": u64::MAX})),
                timestamp_ms: u64::MAX,
                expected_event_sequence: 1,
            },
        )
        .await
        .unwrap();
    let mut expected = observer.load(&trace).await.unwrap().unwrap();
    let original = observer.read_trace(&trace).await.unwrap().0.unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let token = uuid::Uuid::new_v4().to_string();
    let mut killed = OwnedProcess::start(&trace, 1, address, &token);
    let killed_pid = killed.0.id();
    assert_ne!(killed_pid, std::process::id());
    timeout(Duration::from_secs(20), async {
        let (mut channel, _) = listener.accept().await.unwrap();
        let ready = receive(&mut channel).await;
        assert_eq!(ready["pid"], killed_pid);
        assert_eq!(ready["step"], 1);
        assert_eq!(ready["token"], token);
        assert_eq!(ready["raw"], original);
        // No release byte: public append has read its seed but cannot issue EVAL.
        let status = killed.kill_and_reap().await;
        assert_eq!(
            status.signal(),
            Some(9),
            "actual SIGKILL, not assertion exit"
        );
        assert_eq!(killed.0.try_wait().unwrap(), Some(status));
        drop(channel);
        let fresh = timeout(Duration::from_secs(5), RedisPersistenceStore::connect(&url))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fresh.load(&trace).await.unwrap().unwrap(), expected);
        let mut writer = OwnedProcess::start(&trace, 2, address, &token);
        let fresh_pid = writer.0.id();
        assert_ne!(fresh_pid, std::process::id());
        let (mut channel, _) = listener.accept().await.unwrap();
        let ready = receive(&mut channel).await;
        assert_eq!(ready["pid"], fresh_pid);
        assert_eq!(ready["step"], 2);
        assert_eq!(ready["token"], token);
        assert_eq!(ready["raw"], original);
        channel.write_u8(1).await.unwrap();
        let outcome = receive(&mut channel).await;
        assert_eq!(outcome["pid"], fresh_pid);
        assert_eq!(outcome["step"], 2);
        assert_eq!(outcome["outcome"], "ack");
        writer.finish().await;
        let accepted = event(&trace, 1, 2);
        expected.schematic_version = accepted.schematic_version.clone();
        expected.events.push(accepted);
        assert_eq!(fresh.load(&trace).await.unwrap().unwrap(), expected);
        fresh.append(terminal(&trace, 2)).await.unwrap();
        expected.schematic_version = "process-terminal".into();
        expected.events.push(terminal(&trace, 2));
        fresh
            .complete(&trace, CompletionState::Success)
            .await
            .unwrap();
        expected.completion = Some(CompletionState::Success);
        assert!(
            fresh
                .resume(&trace, 2)
                .await
                .unwrap_err()
                .to_string()
                .contains("already completed")
        );
        assert_eq!(fresh.load(&trace).await.unwrap().unwrap(), expected);

        println!(
            "Redis kill identities: parent={} killed={killed_pid} fresh={fresh_pid}",
            std::process::id()
        );
    })
    .await
    .expect("bounded pre-EVAL kill/recovery fixture");
    println!("REDIS_KILL_PASS pre-eval SIGKILL9 reaped unchanged-seed fresh-process-ack");
}
