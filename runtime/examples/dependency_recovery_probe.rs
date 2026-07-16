//! Opt-in M419-RQ8 live dependency and process-recovery probe.
//!
//! This example is orchestrated by `scripts/dependency_failure_smoke_podman.ps1`.
//! It is not a production retry loop or a general-purpose administration tool.

#[cfg(all(feature = "persistence-postgres", feature = "persistence-redis"))]
use anyhow::{Context, Result, anyhow, bail};
#[cfg(all(feature = "persistence-postgres", feature = "persistence-redis"))]
use ranvier_runtime::persistence::{
    CompensationIdempotencyStore, CompletionState, PersistenceEnvelope, PersistenceStore,
    PostgresCompensationIdempotencyStore, PostgresPersistenceStore, RedisPersistenceStore,
};
#[cfg(all(feature = "persistence-postgres", feature = "persistence-redis"))]
use std::io::Write;
#[cfg(all(feature = "persistence-postgres", feature = "persistence-redis"))]
use std::path::{Path, PathBuf};
#[cfg(all(feature = "persistence-postgres", feature = "persistence-redis"))]
use std::time::Duration;

#[cfg(all(feature = "persistence-postgres", feature = "persistence-redis"))]
const CRASH_EXIT_CODE: i32 = 86;

#[cfg(all(feature = "persistence-postgres", feature = "persistence-redis"))]
fn envelope(trace_id: &str, step: u64, outcome_kind: &str) -> PersistenceEnvelope {
    PersistenceEnvelope {
        trace_id: trace_id.to_string(),
        circuit: "Rq8RecoveryProbe".to_string(),
        schematic_version: "rq8-v1".to_string(),
        step,
        node_id: Some(format!("step-{step}")),
        outcome_kind: outcome_kind.to_string(),
        timestamp_ms: 1_000 + step,
        payload_hash: None,
        payload: Some(serde_json::json!({ "step": step })),
    }
}

#[cfg(all(feature = "persistence-postgres", feature = "persistence-redis"))]
fn validated_run_id(raw: &str) -> Result<String> {
    if raw.is_empty()
        || !raw
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        bail!("run id must contain only ASCII letters, digits, or underscore");
    }
    Ok(raw.to_string())
}

#[cfg(all(feature = "persistence-postgres", feature = "persistence-redis"))]
fn write_marker(control_dir: &Path, name: &str) -> Result<()> {
    std::fs::write(control_dir.join(name), b"ok\n")
        .with_context(|| format!("write control marker {name}"))
}

#[cfg(all(feature = "persistence-postgres", feature = "persistence-redis"))]
async fn wait_for_marker(control_dir: &Path, name: &str) -> Result<()> {
    let marker = control_dir.join(name);
    for _ in 0..60 {
        if marker.is_file() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err(anyhow!("timed out waiting for marker {}", marker.display()))
}

#[cfg(all(feature = "persistence-postgres", feature = "persistence-redis"))]
async fn postgres_pool() -> Result<sqlx::PgPool> {
    let url = std::env::var("RANVIER_PERSISTENCE_POSTGRES_URL")
        .context("RANVIER_PERSISTENCE_POSTGRES_URL is required")?;
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(Duration::from_secs(2))
        .connect(&url)
        .await
        .context("connect PostgreSQL probe pool")
}

#[cfg(all(feature = "persistence-postgres", feature = "persistence-redis"))]
async fn postgres_live(control_dir: PathBuf, run_id: String) -> Result<()> {
    std::fs::create_dir_all(&control_dir).context("create PostgreSQL control directory")?;
    let pool = postgres_pool().await?;
    let store = PostgresPersistenceStore::with_table_prefix(pool, format!("rq8_live_{run_id}"));
    store.ensure_schema().await?;
    let trace_id = format!("rq8-postgres-live-{run_id}");
    store.append(envelope(&trace_id, 0, "Enter")).await?;
    if store.load(&trace_id).await?.is_none() {
        bail!("PostgreSQL pre-outage trace was not readable");
    }
    write_marker(&control_dir, "ready")?;
    println!("POSTGRES_READY trace_id={trace_id}");

    wait_for_marker(&control_dir, "inject-outage").await?;
    match tokio::time::timeout(Duration::from_secs(5), store.load(&trace_id)).await {
        Ok(Err(error)) => println!("POSTGRES_OUTAGE_ERROR kind={}", error.root_cause()),
        Err(_) => println!("POSTGRES_OUTAGE_TIMEOUT budget_seconds=5"),
        Ok(Ok(_)) => bail!("PostgreSQL operation unexpectedly succeeded during outage"),
    }
    write_marker(&control_dir, "outage-observed")?;

    wait_for_marker(&control_dir, "inject-recovery").await?;
    for attempt in 1..=30 {
        let recovered = tokio::time::timeout(Duration::from_secs(1), store.load(&trace_id)).await;
        if let Ok(Ok(Some(trace))) = recovered
            && trace.events.len() == 1
            && trace.events[0].outcome_kind == "Enter"
        {
            println!("POSTGRES_SAME_INSTANCE_RECOVERED attempt={attempt}");
            write_marker(&control_dir, "recovered")?;
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    bail!("PostgreSQL store did not recover within 30 attempts")
}

#[cfg(all(feature = "persistence-postgres", feature = "persistence-redis"))]
async fn redis_live(control_dir: PathBuf, run_id: String) -> Result<()> {
    std::fs::create_dir_all(&control_dir).context("create Redis control directory")?;
    let url = std::env::var("RANVIER_PERSISTENCE_REDIS_URL")
        .context("RANVIER_PERSISTENCE_REDIS_URL is required")?;
    let store = RedisPersistenceStore::connect(&url).await?;
    let trace_id = format!("rq8-redis-live-{run_id}");
    store.append(envelope(&trace_id, 0, "Enter")).await?;
    if store.load(&trace_id).await?.is_none() {
        bail!("Redis pre-outage trace was not readable");
    }
    write_marker(&control_dir, "ready")?;
    println!("REDIS_READY trace_id={trace_id}");

    wait_for_marker(&control_dir, "inject-outage").await?;
    match tokio::time::timeout(Duration::from_secs(5), store.load(&trace_id)).await {
        Ok(Err(error)) => println!("REDIS_OUTAGE_ERROR kind={}", error.root_cause()),
        Err(_) => println!("REDIS_OUTAGE_TIMEOUT budget_seconds=5"),
        Ok(Ok(_)) => bail!("Redis operation unexpectedly succeeded during outage"),
    }
    write_marker(&control_dir, "outage-observed")?;

    wait_for_marker(&control_dir, "inject-recovery").await?;
    let recovery_trace_id = format!("rq8-redis-recovered-{run_id}");
    for attempt in 1..=30 {
        let recovered = tokio::time::timeout(
            Duration::from_secs(1),
            store.append(envelope(&recovery_trace_id, 0, "Enter")),
        )
        .await;
        if matches!(recovered, Ok(Ok(()))) {
            println!("REDIS_SAME_MANAGER_RECOVERED attempt={attempt}");
            write_marker(&control_dir, "recovered")?;
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    bail!("Redis connection manager did not recover within 30 attempts")
}

#[cfg(all(feature = "persistence-postgres", feature = "persistence-redis"))]
async fn postgres_crash_write(run_id: String) -> Result<()> {
    let pool = postgres_pool().await?;
    let prefix = format!("rq8_crash_{run_id}");
    let store = PostgresPersistenceStore::with_table_prefix(pool.clone(), &prefix);
    let idempotency = PostgresCompensationIdempotencyStore::with_table_prefix(pool, &prefix);
    store.ensure_schema().await?;
    idempotency.ensure_schema().await?;
    let trace_id = format!("rq8-postgres-crash-{run_id}");
    let idempotency_key = format!("{trace_id}:Rq8RecoveryProbe:Fault");

    store.append(envelope(&trace_id, 0, "Enter")).await?;
    store.append(envelope(&trace_id, 1, "Fault")).await?;
    idempotency.mark_compensated(&idempotency_key).await?;
    let trace = store
        .load(&trace_id)
        .await?
        .ok_or_else(|| anyhow!("crash checkpoint missing before process exit"))?;
    if trace.events.len() != 2 || trace.completion.is_some() {
        bail!("unexpected pre-crash trace state");
    }
    if !idempotency.was_compensated(&idempotency_key).await? {
        bail!("idempotency marker missing before process exit");
    }

    println!("POSTGRES_CRASH_BOUNDARY_COMMITTED trace_id={trace_id} next_step=2");
    std::io::stdout()
        .flush()
        .context("flush crash-boundary evidence marker")?;
    std::process::exit(CRASH_EXIT_CODE);
}

#[cfg(all(feature = "persistence-postgres", feature = "persistence-redis"))]
async fn postgres_crash_recover(run_id: String) -> Result<()> {
    let pool = postgres_pool().await?;
    let prefix = format!("rq8_crash_{run_id}");
    let store = PostgresPersistenceStore::with_table_prefix(pool.clone(), &prefix);
    let idempotency = PostgresCompensationIdempotencyStore::with_table_prefix(pool, &prefix);
    let trace_id = format!("rq8-postgres-crash-{run_id}");
    let idempotency_key = format!("{trace_id}:Rq8RecoveryProbe:Fault");

    let trace = store
        .load(&trace_id)
        .await?
        .ok_or_else(|| anyhow!("crash checkpoint missing after process restart"))?;
    let last_step = trace
        .events
        .last()
        .map(|event| event.step)
        .ok_or_else(|| anyhow!("recovered trace has no events"))?;
    if last_step != 1 || trace.completion.is_some() {
        bail!("recovered trace does not match committed non-terminal checkpoint");
    }
    let cursor = store.resume(&trace_id, last_step).await?;
    if cursor.next_step != 2 {
        bail!("unexpected resume cursor after process restart");
    }
    if !idempotency.was_compensated(&idempotency_key).await? {
        bail!("durable compensation idempotency marker missing after restart");
    }

    let completed_id = format!("rq8-postgres-completed-{run_id}");
    store.append(envelope(&completed_id, 0, "Enter")).await?;
    store
        .complete(&completed_id, CompletionState::Success)
        .await?;
    if store.resume(&completed_id, 0).await.is_ok() {
        bail!("completed PostgreSQL trace unexpectedly resumed");
    }

    println!("POSTGRES_PROCESS_RECOVERY_OK trace_id={trace_id} next_step=2");
    println!("POSTGRES_COMPENSATION_MARKER_RECOVERED key={idempotency_key}");
    println!("POSTGRES_COMPLETED_RESUME_REJECTED trace_id={completed_id}");
    Ok(())
}

#[cfg(all(feature = "persistence-postgres", feature = "persistence-redis"))]
#[tokio::main]
async fn main() -> Result<()> {
    let mut arguments = std::env::args().skip(1);
    let mode = arguments.next().context("missing probe mode")?;
    match mode.as_str() {
        "postgres-live" => {
            let control_dir = arguments.next().context("missing control directory")?;
            let run_id = validated_run_id(&arguments.next().context("missing run id")?)?;
            postgres_live(PathBuf::from(control_dir), run_id).await
        }
        "redis-live" => {
            let control_dir = arguments.next().context("missing control directory")?;
            let run_id = validated_run_id(&arguments.next().context("missing run id")?)?;
            redis_live(PathBuf::from(control_dir), run_id).await
        }
        "postgres-crash-write" => {
            let run_id = validated_run_id(&arguments.next().context("missing run id")?)?;
            postgres_crash_write(run_id).await
        }
        "postgres-crash-recover" => {
            let run_id = validated_run_id(&arguments.next().context("missing run id")?)?;
            postgres_crash_recover(run_id).await
        }
        _ => bail!("unknown probe mode: {mode}"),
    }
}

#[cfg(not(all(feature = "persistence-postgres", feature = "persistence-redis")))]
fn main() {
    eprintln!("enable persistence-postgres and persistence-redis to run this probe");
    std::process::exit(2);
}
