use ranvier_core::cancellation::{CancellationReason, CancellationToken};
use ranvier_core::schematic::Schematic;
use ranvier_inspector::Inspector;
use std::{process::Stdio, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

#[tokio::test]
#[ignore = "requires explicit Node and maintained VSCode shutdown verifier paths"]
async fn managed_inspector_joins_active_idle_socket_on_shutdown() {
    verify_managed_shutdown(1).await;
}

#[tokio::test]
#[ignore = "requires explicit Node and maintained VSCode shutdown verifier paths"]
async fn managed_inspector_joins_multiple_active_sockets_on_shutdown() {
    verify_managed_shutdown(8).await;
}

async fn verify_managed_shutdown(count: usize) {
    let node = std::env::var_os("RANVIER_CONTRACT_NODE").expect("explicit Node required");
    let script = std::env::var_os("RANVIER_STREAM_SHUTDOWN_SCRIPT")
        .expect("explicit shutdown script required");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let inspector = Inspector::new(Schematic::new("idle-shutdown-fixture"), port)
        .with_mode("dev")
        .with_bearer_token("fixture-shutdown-token")
        .with_auth_enforcement(false)
        .with_require_tenant_for_internal(false);
    let cancellation = CancellationToken::new();
    let server_token = cancellation.clone();
    let server = tokio::spawn(async move {
        inspector
            .serve_with_listener_and_cancellation(listener, server_token)
            .await
    });
    let spawned = tokio::process::Command::new(node)
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
        .env("RANVIER_STREAM_FIXTURE_TOKEN", "fixture-shutdown-token")
        .env("RANVIER_STREAM_SHUTDOWN_COUNT", count.to_string())
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(_) => {
            cancellation.cancel(CancellationReason::OperatorShutdown);
            let _ = tokio::time::timeout(Duration::from_secs(3), server).await;
            panic!("shutdown verifier spawn failed");
        }
    };
    let mut lines = BufReader::new(child.stdout.take().unwrap().take(16384)).lines();
    let stderr = child.stderr.take().unwrap();
    let errors = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stderr.take(65536).read_to_end(&mut bytes).await.unwrap();
        bytes
    });
    let ready = tokio::time::timeout(Duration::from_secs(7), lines.next_line()).await;
    cancellation.cancel(CancellationReason::OperatorShutdown);
    let shutdown = tokio::time::timeout(Duration::from_secs(3), server).await;
    let closed = tokio::time::timeout(Duration::from_secs(3), lines.next_line()).await;
    if ready.is_err() || closed.is_err() {
        let _ = child.kill().await;
    }
    let status = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
    if status.is_err() {
        let _ = child.kill().await;
        let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
    }
    let errors = tokio::time::timeout(Duration::from_secs(3), errors).await;
    assert_eq!(ready.unwrap().unwrap().as_deref(), Some("SHUTDOWN_READY"));
    shutdown.expect("managed server deadline").unwrap().unwrap();
    assert_eq!(
        closed.unwrap().unwrap().as_deref(),
        Some("SHUTDOWN_DONE 1001")
    );
    assert!(
        status
            .expect("natural verifier exit deadline")
            .unwrap()
            .success()
    );
    assert!(errors.unwrap().unwrap().is_empty());
    println!("{count} active idle sockets each received 1001; managed server and verifier joined");
}
