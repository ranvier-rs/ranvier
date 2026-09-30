use ranvier_core::cancellation::{CancellationReason, CancellationToken};
use ranvier_core::schematic::Schematic;
use ranvier_inspector::Inspector;
use std::process::Stdio;
use std::time::Duration;

#[tokio::test]
#[ignore = "requires explicit RANVIER_CONTRACT_NODE and RANVIER_INSPECTOR_CONTRACT_CLI paths plus generated OpenAPI"]
async fn required_cli_checks_actual_bearer_protected_inspector() {
    let node =
        std::env::var_os("RANVIER_CONTRACT_NODE").expect("explicit Node executable required");
    let script =
        std::env::var_os("RANVIER_INSPECTOR_CONTRACT_CLI").expect("explicit contract CLI required");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let inspector = Inspector::new(Schematic::new("authenticated-runtime-proof"), port)
        .with_mode("dev")
        .with_bearer_token("fixture-runtime-token");
    let cancellation = CancellationToken::new();
    let server_token = cancellation.clone();
    let server = tokio::spawn(async move {
        inspector
            .serve_with_listener_and_cancellation(listener, server_token)
            .await
    });
    let url = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let ready = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if client.get(format!("{url}/schematic")).send().await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    let mut results = Vec::new();
    if ready.is_ok() {
        for token in [
            None,
            Some("wrong-fixture-token"),
            Some("fixture-runtime-token"),
        ] {
            let mut command = tokio::process::Command::new(&node);
            command
                .arg(&script)
                .arg(&url)
                .env_remove("RANVIER_INSPECTOR_TOKEN")
                .kill_on_drop(true)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if let Some(token) = token {
                command.env("RANVIER_INSPECTOR_TOKEN", token);
            }
            results.push(tokio::time::timeout(Duration::from_secs(10), command.output()).await);
        }
    }
    cancellation.cancel(CancellationReason::OperatorShutdown);
    tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    ready.expect("Inspector readiness deadline");
    for (index, result) in results.into_iter().enumerate() {
        let output = result
            .expect("managed CLI deadline")
            .expect("CLI execution");
        let log = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!log.contains("fixture-runtime-token"));
        assert!(!log.contains("wrong-fixture-token"));
        if index == 2 {
            assert!(output.status.success(), "{log}");
            assert!(log.contains("required schematic runtime check passed"));
        } else {
            assert_eq!(output.status.code(), Some(1), "{log}");
            assert!(log.contains("HTTP 401"));
        }
    }
}
