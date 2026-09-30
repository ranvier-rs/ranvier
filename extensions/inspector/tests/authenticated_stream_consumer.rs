use futures::FutureExt;
use ranvier_core::cancellation::{CancellationReason, CancellationToken};
use ranvier_core::schematic::Schematic;
use ranvier_inspector::Inspector;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tracing_subscriber::prelude::*;

#[tokio::test]
#[ignore = "requires explicit Node/VSCode stream script paths and 100ms fixture broadcast/stall environment"]
async fn actual_inspector_producers_pass_maintained_stream_consumer() {
    let node = std::env::var_os("RANVIER_CONTRACT_NODE").expect("explicit Node required");
    let script =
        std::env::var_os("RANVIER_STREAM_CONTRACT_SCRIPT").expect("explicit script required");
    assert_eq!(
        std::env::var("RANVIER_INSPECTOR_METRICS_INTERVAL_MS").as_deref(),
        Ok("100")
    );
    assert_eq!(
        std::env::var("RANVIER_INSPECTOR_STALL_THRESHOLD_MS").as_deref(),
        Ok("100")
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let inspector = Inspector::new(Schematic::new("stream-wire-fixture"), port)
        .with_mode("dev")
        .with_bearer_token("fixture-stream-token")
        .with_auth_enforcement(false)
        .with_require_tenant_for_internal(false);
    let cancellation = CancellationToken::new();
    let server_token = cancellation.clone();
    let server = tokio::spawn(async move {
        inspector
            .serve_with_listener_and_cancellation(listener, server_token)
            .await
    });
    let mut command = tokio::process::Command::new(node);
    command
        .args(["--import", "tsx"])
        .arg(&script)
        .current_dir(
            std::path::Path::new(&script)
                .parent()
                .unwrap()
                .parent()
                .unwrap(),
        )
        .arg(format!("http://127.0.0.1:{port}"))
        .env("RANVIER_STREAM_FIXTURE_TOKEN", "fixture-stream-token")
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            cancellation.cancel(CancellationReason::OperatorShutdown);
            let _ = tokio::time::timeout(Duration::from_secs(3), server).await;
            panic!("stream validator process could not start");
        }
    };
    let mut lines = BufReader::new(child.stdout.take().unwrap().take(16384)).lines();
    let stderr = child.stderr.take().unwrap();
    let errors = tokio::spawn(async move {
        let mut data = Vec::new();
        stderr.take(65536).read_to_end(&mut data).await.unwrap();
        data
    });
    let dispatch =
        tracing::Dispatch::new(tracing_subscriber::registry().with(ranvier_inspector::layer()));
    let result = tokio::time::timeout(Duration::from_secs(15), std::panic::AssertUnwindSafe(async {
        assert_eq!(lines.next_line().await.unwrap().as_deref(), Some("WIRE_READY"));
        // A real delivered marker proves the server subscription exists, avoiding
        // a race between the 101 response and handle_socket's subscribe call.
        loop {
            tracing::dispatcher::with_default(&dispatch, || {
                tracing::info!(target: "ranvier.stream_fixture", message = "wire-ready");
            });
            if let Ok(line) = tokio::time::timeout(Duration::from_millis(50), lines.next_line()).await {
                assert_eq!(line.unwrap().as_deref(), Some("WIRE_ARMED"));
                break;
            }
        }
        let (circuit, node) = tracing::dispatcher::with_default(&dispatch, || {
            let circuit = tracing::info_span!(target: "ranvier.stream_fixture", "Circuit", ranvier.circuit = "stream-wire-fixture");
            let _circuit = circuit.enter();
            let node = tracing::info_span!(target: "ranvier.stream_fixture", "Node", ranvier.node = "wire-node");
            node.in_scope(|| {
                tracing::info!(target: "ranvier.debugger", trace_id = "wire-trace", node_id = "wire-node");
            });
            (circuit.clone(), node)
        });
        tokio::time::sleep(Duration::from_millis(250)).await;
        tracing::dispatcher::with_default(&dispatch, || {
            drop(node);
            drop(circuit);
        });
        let line = lines.next_line().await.unwrap().expect("stream report required");
        assert!(line.starts_with("WIRE_DONE "), "unexpected protocol response");
        line
    }).catch_unwind()).await;
    if !matches!(result, Ok(Ok(_))) {
        let _ = child.kill().await;
    }
    let status = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
    let reaped = if status.is_err() {
        let _ = child.kill().await;
        Some(tokio::time::timeout(Duration::from_secs(3), child.wait()).await)
    } else {
        None
    };
    cancellation.cancel(CancellationReason::OperatorShutdown);
    let shutdown = tokio::time::timeout(Duration::from_secs(3), server).await;
    let errors = tokio::time::timeout(Duration::from_secs(3), errors).await;
    shutdown
        .expect("managed Inspector shutdown deadline")
        .unwrap()
        .unwrap();
    if let Some(reaped) = reaped {
        assert!(reaped.is_ok(), "validator kill/reap deadline");
    }
    let status = status.expect("validator natural exit deadline").unwrap();
    let errors = errors.expect("validator stderr join deadline").unwrap();
    assert!(
        status.success(),
        "stream validator process failed: {}",
        String::from_utf8_lossy(&errors)
    );
    assert!(errors.is_empty(), "stream validator emitted stderr");
    let report = result
        .expect("stream execution deadline")
        .expect("stream protocol assertion");
    assert!(!report.contains("fixture-stream-token"));
    let value: serde_json::Value =
        serde_json::from_str(report.strip_prefix("WIRE_DONE ").unwrap()).unwrap();
    assert_eq!(value["count"], 7);
    println!("{report}");
}
