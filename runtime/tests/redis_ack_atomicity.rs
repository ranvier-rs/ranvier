#![cfg(feature = "persistence-redis")]

use anyhow::{Context, Result, ensure};
use ranvier_core::prelude::*;
use ranvier_runtime::{
    Axon, EffectBoundary, ExecutionReport, PersistenceOperation,
    persistence::{
        CompletionState, PersistenceEnvelope, PersistenceHandle, PersistenceStore,
        PersistenceTraceId, RedisPersistenceStore,
    },
};
use std::{
    net::{IpAddr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, lookup_host},
    sync::{mpsc, watch},
    task::{JoinHandle, JoinSet},
    time::{Duration, timeout},
};

const MAX_RESP_FRAME_BYTES: usize = 16 * 1024 * 1024;
const MAX_RESP_LINE_BYTES: usize = 4096;
const MAX_RESP_ELEMENTS: usize = 100_000;

/// Documented v2 marker key for a compensation with a trace store.
fn v2_journaled_marker_key(trace_id: &str, circuit: &str, intent_sequence: u64) -> String {
    format!(
        "v2:j:{intent_sequence}:{}:{trace_id}:{}:{circuit}",
        trace_id.len(),
        circuit.len()
    )
}

/// `event_sequence` of the trace's external-compensation intent.
async fn compensation_intent_sequence(
    store: &impl PersistenceStore,
    trace_id: &str,
) -> Result<u64> {
    let trace = store
        .load(trace_id)
        .await?
        .context("compensation trace was not persisted")?;
    trace
        .events
        .iter()
        .rev()
        .find(|event| {
            event.node_id.as_deref() == Some("external-compensation")
                && event.outcome_kind == "Intent"
        })
        .map(|event| event.event_sequence)
        .context("external compensation intent was not persisted")
}

#[derive(Debug)]
struct DroppedCommandReply {
    command: String,
    ordinal: usize,
    response: Vec<u8>,
}

struct ProxyGuard {
    shutdown: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
}

impl ProxyGuard {
    async fn stop(mut self) -> Result<()> {
        let _ = self.shutdown.send(true);
        self.task
            .take()
            .context("Redis ACK proxy task is missing")?
            .await?;
        Ok(())
    }
}

impl Drop for ProxyGuard {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

async fn read_crlf_line<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Vec<u8>> {
    let mut line = Vec::new();
    loop {
        let byte = reader.read_u8().await?;
        line.push(byte);
        ensure!(
            line.len() <= MAX_RESP_LINE_BYTES,
            "RESP line exceeds fixture bound"
        );
        if line.ends_with(b"\r\n") {
            line.truncate(line.len() - 2);
            return Ok(line);
        }
    }
}

fn push_bounded(frame: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    ensure!(
        frame.len().saturating_add(bytes.len()) <= MAX_RESP_FRAME_BYTES,
        "RESP frame exceeds fixture bound"
    );
    frame.extend_from_slice(bytes);
    Ok(())
}

async fn read_resp_frame<R: AsyncRead + Unpin>(reader: &mut R, depth: usize) -> Result<Vec<u8>> {
    ensure!(depth <= 32, "RESP nesting exceeds fixture bound");
    let kind = reader.read_u8().await?;
    let mut frame = vec![kind];
    match kind {
        b'+' | b'-' | b':' | b',' | b'(' | b'#' | b'_' => {
            let line = read_crlf_line(reader).await?;
            push_bounded(&mut frame, &line)?;
            push_bounded(&mut frame, b"\r\n")?;
        }
        b'$' | b'!' | b'=' => {
            let line = read_crlf_line(reader).await?;
            let length: i64 = std::str::from_utf8(&line)?.parse()?;
            push_bounded(&mut frame, &line)?;
            push_bounded(&mut frame, b"\r\n")?;
            if length >= 0 {
                let length = usize::try_from(length)?;
                ensure!(
                    length <= MAX_RESP_FRAME_BYTES,
                    "RESP bulk exceeds fixture bound"
                );
                let mut body = vec![0; length.saturating_add(2)];
                reader.read_exact(&mut body).await?;
                ensure!(body.ends_with(b"\r\n"), "RESP bulk lacks CRLF terminator");
                push_bounded(&mut frame, &body)?;
            } else {
                ensure!(length == -1, "invalid RESP bulk length");
            }
        }
        b'*' | b'~' | b'>' => {
            let line = read_crlf_line(reader).await?;
            let count: i64 = std::str::from_utf8(&line)?.parse()?;
            push_bounded(&mut frame, &line)?;
            push_bounded(&mut frame, b"\r\n")?;
            if count >= 0 {
                let count = usize::try_from(count)?;
                ensure!(
                    count <= MAX_RESP_ELEMENTS,
                    "RESP array exceeds fixture bound"
                );
                for _ in 0..count {
                    let child = Box::pin(read_resp_frame(reader, depth + 1)).await?;
                    push_bounded(&mut frame, &child)?;
                }
            } else {
                ensure!(count == -1, "invalid RESP aggregate length");
            }
        }
        b'%' | b'|' => {
            let line = read_crlf_line(reader).await?;
            let count: i64 = std::str::from_utf8(&line)?.parse()?;
            push_bounded(&mut frame, &line)?;
            push_bounded(&mut frame, b"\r\n")?;
            ensure!(count >= 0, "invalid RESP map length");
            let count = usize::try_from(count)?;
            ensure!(
                count <= MAX_RESP_ELEMENTS / 2,
                "RESP map exceeds fixture bound"
            );
            for _ in 0..count.saturating_mul(2) {
                let child = Box::pin(read_resp_frame(reader, depth + 1)).await?;
                push_bounded(&mut frame, &child)?;
            }
        }
        _ => anyhow::bail!("unsupported RESP type byte in isolated proxy: {kind}"),
    }
    Ok(frame)
}

async fn read_resp_request<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<Option<(Vec<u8>, String)>> {
    let mut prefix = [0_u8; 1];
    if reader.read(&mut prefix).await? == 0 {
        return Ok(None);
    }
    ensure!(
        prefix[0] == b'*',
        "Redis fixture request is not a RESP array"
    );
    let mut frame = vec![prefix[0]];
    let line = read_crlf_line(reader).await?;
    let count: usize = std::str::from_utf8(&line)?.parse()?;
    ensure!(
        (1..=1024).contains(&count),
        "RESP command arity outside fixture bound"
    );
    push_bounded(&mut frame, &line)?;
    push_bounded(&mut frame, b"\r\n")?;

    let mut command = None;
    for index in 0..count {
        let argument = read_resp_frame(reader, 0).await?;
        if index == 0 {
            ensure!(
                argument.first() == Some(&b'$'),
                "Redis command name is not a bulk string"
            );
            let line_end = argument
                .windows(2)
                .position(|pair| pair == b"\r\n")
                .context("RESP command name length is missing")?;
            let length: usize = std::str::from_utf8(&argument[1..line_end])?.parse()?;
            let start = line_end + 2;
            let end = start.saturating_add(length);
            ensure!(end + 2 <= argument.len(), "RESP command name is truncated");
            command = Some(std::str::from_utf8(&argument[start..end])?.to_owned());
        }
        push_bounded(&mut frame, &argument)?;
    }
    Ok(Some((
        frame,
        command.context("RESP command name is absent")?,
    )))
}

async fn proxy_connection(
    client: TcpStream,
    backend_address: SocketAddr,
    target_command: &'static str,
    target_ordinal: usize,
    command_count: Arc<AtomicUsize>,
    dropped_reply: mpsc::UnboundedSender<DroppedCommandReply>,
) -> Result<()> {
    let backend = TcpStream::connect(backend_address).await?;
    let (mut client_reader, mut client_writer) = client.into_split();
    let (mut backend_reader, mut backend_writer) = backend.into_split();
    loop {
        let Some((request, command)) = read_resp_request(&mut client_reader).await? else {
            return Ok(());
        };
        backend_writer.write_all(&request).await?;
        let response = read_resp_frame(&mut backend_reader, 0).await?;
        let ordinal = command
            .eq_ignore_ascii_case(target_command)
            .then(|| command_count.fetch_add(1, Ordering::SeqCst) + 1);
        if ordinal == Some(target_ordinal) {
            dropped_reply
                .send(DroppedCommandReply {
                    command,
                    ordinal: target_ordinal,
                    response,
                })
                .map_err(|_| anyhow::anyhow!("Redis ACK observation receiver was dropped"))?;
            // The response is deliberately discarded after Redis has executed
            // the command. Closing both sockets prevents stale RESP framing.
            return Ok(());
        }
        client_writer.write_all(&response).await?;
    }
}

async fn run_proxy(
    listener: TcpListener,
    backend_address: SocketAddr,
    target_command: &'static str,
    target_ordinal: usize,
    dropped_reply: mpsc::UnboundedSender<DroppedCommandReply>,
    mut shutdown: watch::Receiver<bool>,
) {
    let command_count = Arc::new(AtomicUsize::new(0));
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((client, _)) = accepted else { break };
                connections.spawn(proxy_connection(
                    client,
                    backend_address,
                    target_command,
                    target_ordinal,
                    command_count.clone(),
                    dropped_reply.clone(),
                ));
            }
            joined = connections.join_next(), if !connections.is_empty() => {
                let _ = joined;
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
        }
    }
    connections.abort_all();
    while connections.join_next().await.is_some() {}
}

async fn start_proxy(
    backend_address: SocketAddr,
    target_eval_ordinal: usize,
) -> Result<(
    String,
    mpsc::UnboundedReceiver<DroppedCommandReply>,
    ProxyGuard,
)> {
    start_proxy_for_command(backend_address, "EVAL", target_eval_ordinal).await
}

async fn start_proxy_for_command(
    backend_address: SocketAddr,
    target_command: &'static str,
    target_ordinal: usize,
) -> Result<(
    String,
    mpsc::UnboundedReceiver<DroppedCommandReply>,
    ProxyGuard,
)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (dropped_tx, dropped_rx) = mpsc::unbounded_channel();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let task = tokio::spawn(run_proxy(
        listener,
        backend_address,
        target_command,
        target_ordinal,
        dropped_tx,
        shutdown_rx,
    ));
    Ok((
        format!("redis://{address}"),
        dropped_rx,
        ProxyGuard {
            shutdown: shutdown_tx,
            task: Some(task),
        },
    ))
}

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

#[tokio::test]
#[ignore = "requires explicitly isolated unauthenticated Redis fixture URL and address"]
async fn redis_result_append_ack_lost_after_tcp_eval_keeps_report_and_committed_trace() -> Result<()>
{
    timeout(Duration::from_secs(20), async {
        let backend_url = std::env::var("RANVIER_ATOMICITY_REDIS_URL")
            .context("explicit isolated Redis URL required")?;
        ensure!(
            backend_url.starts_with("redis://") && !backend_url.contains('@'),
            "fixture supports only an isolated unauthenticated redis:// endpoint"
        );
        let backend_endpoint = std::env::var("RANVIER_ATOMICITY_REDIS_ADDRESS")
            .context("explicit isolated Redis address required")?;
        let backend_address = lookup_host(&backend_endpoint)
            .await?
            .next()
            .context("isolated Redis address did not resolve")?;
        let reader = RedisPersistenceStore::connect(&backend_url).await?;
        let trace_id = format!("redis-result-ack-{}", uuid::Uuid::new_v4().simple());
        ensure!(reader.load(&trace_id).await?.is_none(), "unique trace id already exists");

        // Execution sends EVAL #1 for Enter, #2 for the write-ahead Intent,
        // and #3 for this transition's Next result. Drop the result reply.
        let (proxy_url, mut dropped_reply, proxy) = start_proxy(backend_address, 3).await?;
        let execution_store = RedisPersistenceStore::connect(&proxy_url).await?;
        let effects = Arc::new(AtomicUsize::new(0));
        let axon = Axon::<u32, u32, String, ()>::new("RedisResultAckLossFixture")
            .then(CountEffect(effects.clone()));
        let mut bus = Bus::new();
        bus.insert(PersistenceHandle::from_store(execution_store));
        bus.insert(PersistenceTraceId::new(trace_id.clone()));

        let report = axon.execute(10, &(), &mut bus).await;
        let observed_drop = timeout(Duration::from_secs(5), dropped_reply.recv())
            .await?
            .context("Redis TCP proxy did not drop the targeted EVAL reply")?;
        ensure!(
            observed_drop.command.eq_ignore_ascii_case("EVAL") && observed_drop.ordinal == 3,
            "proxy dropped a different Redis command"
        );
        ensure!(observed_drop.response == b":1\r\n", "Redis did not commit the targeted EVAL");
        ensure!(
            matches!(
                &report,
                ExecutionReport::Unconfirmed {
                    result: Outcome::Next(11),
                    operation: PersistenceOperation::Result,
                    effect: EffectBoundary::MayHaveRun,
                    ..
                }
            ),
            "unexpected caller report after losing committed result ACK: {report:?}"
        );
        ensure!(effects.load(Ordering::SeqCst) == 1, "domain effect did not run exactly once");

        let trace = reader
            .load(&trace_id)
            .await?
            .context("fresh Redis client could not read the trace")?;
        ensure!(trace.events.len() == 3, "unexpected persisted event count");
        ensure!(trace.events[0].outcome_kind == "Enter", "initial event was not retained");
        ensure!(trace.events[1].outcome_kind == "Intent", "write-ahead intent is missing");
        ensure!(trace.events[2].outcome_kind == "Next", "committed result is missing");
        ensure!(trace.events[2].step == 1, "result event has an unexpected step");
        ensure!(
            trace.events[2].payload == Some(Outcome::<u32, String>::next(11).to_json_value()),
            "committed result payload differs from the caller outcome"
        );
        ensure!(trace.completion.is_none(), "unconfirmed result was marked complete");
        proxy.stop().await?;
        println!(
            "REDIS_TCP_RESULT_ACK_LOST eval={} reply=:1 effects=1 stored=[Enter,Intent,Next] completion=unset",
            observed_drop.ordinal
        );
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("isolated Redis TCP result ACK-loss fixture must finish within 20 seconds")??;
    Ok(())
}

#[tokio::test]
#[ignore = "requires explicitly isolated unauthenticated Redis fixture URL and address"]
async fn redis_terminal_ack_loss_over_tcp_blocks_reentry() -> Result<()> {
    timeout(Duration::from_secs(20), async {
        let backend_url = std::env::var("RANVIER_ATOMICITY_REDIS_URL")
            .context("explicit isolated Redis URL required")?;
        ensure!(
            backend_url.starts_with("redis://") && !backend_url.contains('@'),
            "fixture supports only an isolated unauthenticated redis:// endpoint"
        );
        let backend_endpoint = std::env::var("RANVIER_ATOMICITY_REDIS_ADDRESS")
            .context("explicit isolated Redis address required")?;
        let backend_address = lookup_host(&backend_endpoint)
            .await?
            .next()
            .context("isolated Redis address did not resolve")?;
        let reader = RedisPersistenceStore::connect(&backend_url).await?;
        let trace_id = format!("redis-terminal-ack-{}", uuid::Uuid::new_v4().simple());
        ensure!(reader.load(&trace_id).await?.is_none(), "unique trace id already exists");

        // EVAL #1 appends Enter, #2 Intent, #3 the transition result, and #4
        // the terminal Next. Drop only the successful terminal reply.
        let (proxy_url, mut dropped_reply, proxy) = start_proxy(backend_address, 4).await?;
        let execution_store = RedisPersistenceStore::connect(&proxy_url).await?;
        let effects = Arc::new(AtomicUsize::new(0));
        let axon = Axon::<u32, u32, String, ()>::new("RedisTerminalAckLossFixture")
            .then(CountEffect(effects.clone()));
        let mut bus = Bus::new();
        bus.insert(PersistenceHandle::from_store(execution_store));
        bus.insert(PersistenceTraceId::new(trace_id.clone()));

        let report = axon.execute(10, &(), &mut bus).await;
        let observed_drop = timeout(Duration::from_secs(5), dropped_reply.recv())
            .await?
            .context("Redis TCP proxy did not drop the targeted EVAL reply")?;
        ensure!(
            observed_drop.command.eq_ignore_ascii_case("EVAL") && observed_drop.ordinal == 4,
            "proxy dropped a different Redis command"
        );
        ensure!(observed_drop.response == b":1\r\n", "Redis did not commit the targeted EVAL");
        ensure!(
            matches!(
                &report,
                ExecutionReport::Unconfirmed {
                    result: Outcome::Next(11),
                    operation: PersistenceOperation::Terminal,
                    effect: EffectBoundary::MayHaveRun,
                    ..
                }
            ),
            "unexpected caller report after losing committed terminal ACK: {report:?}"
        );
        ensure!(effects.load(Ordering::SeqCst) == 1, "domain effect did not run exactly once");

        let trace = reader
            .load(&trace_id)
            .await?
            .context("fresh Redis client could not read the trace")?;
        ensure!(trace.events.len() == 4, "unexpected persisted event count");
        ensure!(trace.events[0].outcome_kind == "Enter", "initial event was not retained");
        ensure!(trace.events[1].outcome_kind == "Intent", "write-ahead intent is missing");
        ensure!(trace.events[2].outcome_kind == "Next", "transition result is missing");
        ensure!(trace.events[3].outcome_kind == "Next", "applied terminal event is missing");
        ensure!(trace.events[2].step == 1, "transition result has an unexpected step");
        ensure!(trace.events[3].step == 2, "terminal event is not after the graph cursor");
        ensure!(trace.completion.is_none(), "unconfirmed terminal was marked complete");
        proxy.stop().await?;

        let mut reentry_bus = Bus::new();
        reentry_bus.insert(PersistenceHandle::from_store(reader.clone()));
        reentry_bus.insert(PersistenceTraceId::new(trace_id.clone()));
        let reentry = axon.execute(10, &(), &mut reentry_bus).await;
        ensure!(
            matches!(
                &reentry,
                ExecutionReport::BlockedBeforeEffect {
                    operation: PersistenceOperation::Terminal,
                    effect: EffectBoundary::NotInvoked,
                    ..
                }
            ),
            "terminal cursor re-entry was not blocked before effects: {reentry:?}"
        );
        ensure!(effects.load(Ordering::SeqCst) == 1, "re-entry repeated the domain effect");
        let after_reentry = reader
            .load(&trace_id)
            .await?
            .context("Redis trace disappeared after re-entry")?;
        ensure!(after_reentry == trace, "re-entry changed the persisted trace");
        println!(
            "REDIS_TCP_TERMINAL_ACK_LOST eval={} reply=:1 effects=1 stored=[Enter,Intent,Next,Next] completion=unset reentry=blocked",
            observed_drop.ordinal
        );
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("isolated Redis TCP terminal ACK-loss fixture must finish within 20 seconds")??;
    Ok(())
}

#[tokio::test]
#[ignore = "requires explicitly isolated unauthenticated Redis fixture URL and address"]
async fn redis_complete_ack_loss_over_tcp_reports_uncertainty_after_apply() -> Result<()> {
    timeout(Duration::from_secs(20), async {
        let backend_url = std::env::var("RANVIER_ATOMICITY_REDIS_URL")
            .context("explicit isolated Redis URL required")?;
        ensure!(
            backend_url.starts_with("redis://") && !backend_url.contains('@'),
            "fixture supports only an isolated unauthenticated redis:// endpoint"
        );
        let backend_endpoint = std::env::var("RANVIER_ATOMICITY_REDIS_ADDRESS")
            .context("explicit isolated Redis address required")?;
        let backend_address = lookup_host(&backend_endpoint)
            .await?
            .next()
            .context("isolated Redis address did not resolve")?;
        let reader = RedisPersistenceStore::connect(&backend_url).await?;
        let trace_id = format!("redis-complete-ack-{}", uuid::Uuid::new_v4().simple());
        ensure!(reader.load(&trace_id).await?.is_none(), "unique trace id already exists");

        // EVAL #1 appends Enter, #2 Intent, #3 the transition result, #4 the
        // terminal Next, and #5 writes completion. Drop the completion reply.
        let (proxy_url, mut dropped_reply, proxy) = start_proxy(backend_address, 5).await?;
        let execution_store = RedisPersistenceStore::connect(&proxy_url).await?;
        let effects = Arc::new(AtomicUsize::new(0));
        let axon = Axon::<u32, u32, String, ()>::new("RedisCompleteAckLossFixture")
            .then(CountEffect(effects.clone()));
        let mut bus = Bus::new();
        bus.insert(PersistenceHandle::from_store(execution_store));
        bus.insert(PersistenceTraceId::new(trace_id.clone()));

        let report = axon.execute(10, &(), &mut bus).await;
        let observed_drop = timeout(Duration::from_secs(5), dropped_reply.recv())
            .await?
            .context("Redis TCP proxy did not drop the targeted EVAL reply")?;
        ensure!(
            observed_drop.command.eq_ignore_ascii_case("EVAL") && observed_drop.ordinal == 5,
            "proxy dropped a different Redis command"
        );
        ensure!(observed_drop.response == b":1\r\n", "Redis did not apply completion");
        ensure!(
            matches!(
                &report,
                ExecutionReport::Unconfirmed {
                    result: Outcome::Next(11),
                    operation: PersistenceOperation::Complete,
                    effect: EffectBoundary::MayHaveRun,
                    ..
                }
            ),
            "unexpected caller report after losing committed completion ACK: {report:?}"
        );
        ensure!(effects.load(Ordering::SeqCst) == 1, "domain effect did not run exactly once");

        let trace = reader
            .load(&trace_id)
            .await?
            .context("separate Redis client could not read the trace")?;
        ensure!(trace.events.len() == 4, "unexpected persisted event count");
        ensure!(trace.events[0].outcome_kind == "Enter", "initial event was not retained");
        ensure!(trace.events[1].outcome_kind == "Intent", "write-ahead intent is missing");
        ensure!(trace.events[2].outcome_kind == "Next", "transition result is missing");
        ensure!(trace.events[3].outcome_kind == "Next", "terminal event is missing");
        ensure!(trace.events[2].step == 1, "transition result has an unexpected step");
        ensure!(trace.events[3].step == 2, "terminal event is not after the graph cursor");
        ensure!(
            trace.completion == Some(CompletionState::Success),
            "successful completion was not applied before its reply was dropped"
        );
        proxy.stop().await?;
        println!(
            "REDIS_TCP_COMPLETE_ACK_LOST eval={} reply=:1 effects=1 stored=[Enter,Intent,Next,Next] completion=Success",
            observed_drop.ordinal
        );
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("isolated Redis TCP completion ACK-loss fixture must finish within 20 seconds")??;
    Ok(())
}

#[tokio::test]
#[ignore = "requires explicitly isolated unauthenticated Redis fixture URL and address"]
async fn redis_load_ack_loss_over_tcp_blocks_before_effect_and_preserves_trace() -> Result<()> {
    timeout(Duration::from_secs(20), async {
        let backend_url = std::env::var("RANVIER_ATOMICITY_REDIS_URL")
            .context("explicit isolated Redis URL required")?;
        ensure!(
            backend_url.starts_with("redis://") && !backend_url.contains('@'),
            "fixture supports only an isolated unauthenticated redis:// endpoint"
        );
        let backend_endpoint = std::env::var("RANVIER_ATOMICITY_REDIS_ADDRESS")
            .context("explicit isolated Redis address required")?;
        let backend_address = lookup_host(&backend_endpoint)
            .await?
            .next()
            .context("isolated Redis address did not resolve")?;
        let reader = RedisPersistenceStore::connect(&backend_url).await?;
        let trace_id = format!("redis-load-ack-{}", uuid::Uuid::new_v4().simple());
        ensure!(reader.load(&trace_id).await?.is_none(), "unique trace id already exists");

        reader
            .append(PersistenceEnvelope {
                trace_id: trace_id.clone(),
                circuit: "RedisLoadAckLossFixture".to_string(),
                schematic_version: "1.0".to_string(),
                event_sequence: 0,
                event_type: ranvier_runtime::persistence::PersistenceEventType::Enter,
                step: 0,
                node_id: None,
                outcome_kind: "Enter".to_string(),
                timestamp_ms: 1,
                payload_hash: None,
                payload: None,
            })
            .await?;
        let before = reader
            .load(&trace_id)
            .await?
            .context("seeded Redis trace could not be read")?;
        ensure!(before.events.len() == 1, "fixture did not seed exactly one Enter event");
        ensure!(before.events[0].outcome_kind == "Enter", "fixture seed is not Enter");

        let (proxy_url, mut dropped_reply, proxy) =
            start_proxy_for_command(backend_address, "GET", 1).await?;
        let execution_store = RedisPersistenceStore::connect(&proxy_url).await?;
        let effects = Arc::new(AtomicUsize::new(0));
        let axon = Axon::<u32, u32, String, ()>::new("RedisLoadAckLossFixture")
            .then(CountEffect(effects.clone()));
        let mut bus = Bus::new();
        bus.insert(PersistenceHandle::from_store(execution_store));
        bus.insert(PersistenceTraceId::new(trace_id.clone()));

        let report = axon.execute(10, &(), &mut bus).await;
        let observed_drop = timeout(Duration::from_secs(5), dropped_reply.recv())
            .await?
            .context("Redis TCP proxy did not drop the targeted GET reply")?;
        ensure!(
            observed_drop.command.eq_ignore_ascii_case("GET") && observed_drop.ordinal == 1,
            "proxy dropped a different Redis command"
        );
        ensure!(
            observed_drop.response.starts_with(b"$")
                && !observed_drop.response.starts_with(b"$-1\r\n"),
            "Redis GET did not return the seeded trace"
        );
        ensure!(
            matches!(
                &report,
                ExecutionReport::BlockedBeforeEffect {
                    operation: PersistenceOperation::Load,
                    effect: EffectBoundary::NotInvoked,
                    ..
                }
            ),
            "unexpected caller report after losing the persisted-trace GET reply: {report:?}"
        );
        ensure!(effects.load(Ordering::SeqCst) == 0, "domain effect ran after load ACK loss");

        let after = reader
            .load(&trace_id)
            .await?
            .context("persisted Redis trace disappeared after the load failure")?;
        ensure!(after == before, "load ACK loss changed the persisted trace");
        ensure!(after.completion.is_none(), "load ACK loss marked the trace complete");
        proxy.stop().await?;
        println!(
            "REDIS_TCP_LOAD_ACK_LOST command={} ordinal={} effects=0 stored=[Enter] completion=unset",
            observed_drop.command, observed_drop.ordinal
        );
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("isolated Redis TCP load ACK-loss fixture must finish within 20 seconds")??;
    Ok(())
}

#[tokio::test]
#[ignore = "requires explicitly isolated unauthenticated Redis fixture URL and address"]
async fn redis_intent_ack_loss_over_tcp_blocks_before_effect_and_preserves_enter() -> Result<()> {
    timeout(Duration::from_secs(20), async {
        let backend_url = std::env::var("RANVIER_ATOMICITY_REDIS_URL")
            .context("explicit isolated Redis URL required")?;
        ensure!(
            backend_url.starts_with("redis://") && !backend_url.contains('@'),
            "fixture supports only an isolated unauthenticated redis:// endpoint"
        );
        let backend_endpoint = std::env::var("RANVIER_ATOMICITY_REDIS_ADDRESS")
            .context("explicit isolated Redis address required")?;
        let backend_address = lookup_host(&backend_endpoint)
            .await?
            .next()
            .context("isolated Redis address did not resolve")?;
        let reader = RedisPersistenceStore::connect(&backend_url).await?;
        let trace_id = format!("redis-intent-ack-{}", uuid::Uuid::new_v4().simple());
        ensure!(
            reader.load(&trace_id).await?.is_none(),
            "unique trace id already exists"
        );

        let (proxy_url, mut dropped_reply, proxy) =
            start_proxy_for_command(backend_address, "EVAL", 1).await?;
        let execution_store = RedisPersistenceStore::connect(&proxy_url).await?;
        let effects = Arc::new(AtomicUsize::new(0));
        let axon = Axon::<u32, u32, String, ()>::new("RedisIntentAckLossFixture")
            .then(CountEffect(effects.clone()));
        let mut bus = Bus::new();
        bus.insert(PersistenceHandle::from_store(execution_store));
        bus.insert(PersistenceTraceId::new(trace_id.clone()));

        let report = axon.execute(10, &(), &mut bus).await;
        let observed_drop = timeout(Duration::from_secs(5), dropped_reply.recv())
            .await?
            .context("Redis TCP proxy did not drop the targeted EVAL reply")?;
        ensure!(
            observed_drop.command.eq_ignore_ascii_case("EVAL") && observed_drop.ordinal == 1,
            "proxy dropped a different Redis command"
        );
        ensure!(
            observed_drop.response == b":1\r\n",
            "Redis did not commit the targeted EVAL"
        );
        ensure!(
            matches!(
                &report,
                ExecutionReport::BlockedBeforeEffect {
                    operation: PersistenceOperation::Intent,
                    effect: EffectBoundary::NotInvoked,
                    ..
                }
            ),
            "unexpected caller report after losing the persisted-intent EVAL reply: {report:?}"
        );
        ensure!(
            effects.load(Ordering::SeqCst) == 0,
            "domain effect ran after intent ACK loss"
        );

        let after = reader
            .load(&trace_id)
            .await?
            .context("committed Redis intent disappeared after its ACK was lost")?;
        ensure!(
            after.events.len() == 1,
            "intent ACK loss stored unexpected extra events"
        );
        ensure!(
            after.events[0].step == 0,
            "stored intent has an unexpected step"
        );
        ensure!(
            after.events[0].outcome_kind == "Enter",
            "stored event is not the expected Enter intent"
        );
        ensure!(
            after.completion.is_none(),
            "intent ACK loss marked the trace complete"
        );
        proxy.stop().await?;
        println!(
            "REDIS_TCP_INTENT_ACK_LOST eval={} reply=:1 effects=0 stored=[Enter] completion=unset",
            observed_drop.ordinal
        );
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("isolated Redis TCP intent ACK-loss fixture must finish within 20 seconds")??;
    Ok(())
}

#[tokio::test]
#[ignore = "requires explicitly isolated unauthenticated Redis fixture URL and address"]
async fn redis_compensation_marker_ack_loss_over_tcp_reports_uncertainty() -> Result<()> {
    use ranvier_runtime::persistence::{
        CompensationHandle, CompensationHook, CompensationIdempotencyHandle,
        CompensationIdempotencyStore, InMemoryPersistenceStore, PersistenceHandle,
        PersistenceTraceId, RedisCompensationIdempotencyStore,
    };

    #[derive(Clone)]
    struct CountFault(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl Transition<u32, u32> for CountFault {
        type Error = String;
        type Resources = ();
        async fn run(&self, _: u32, _: &(), _: &mut Bus) -> Outcome<u32, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Outcome::Fault("business-fault".into())
        }
    }

    #[derive(Clone)]
    struct CountCompensation(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl CompensationHook for CountCompensation {
        async fn compensate(
            &self,
            _: ranvier_runtime::persistence::CompensationContext,
        ) -> anyhow::Result<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    timeout(Duration::from_secs(20), async {
        let backend_url = std::env::var("RANVIER_ATOMICITY_REDIS_URL")
            .context("explicit isolated Redis URL required")?;
        ensure!(
            backend_url.starts_with("redis://") && !backend_url.contains('@'),
            "fixture supports only an isolated unauthenticated redis:// endpoint"
        );
        let backend_endpoint = std::env::var("RANVIER_ATOMICITY_REDIS_ADDRESS")
            .context("explicit isolated Redis address required")?;
        let backend_address = lookup_host(&backend_endpoint)
            .await?
            .next()
            .context("isolated Redis address did not resolve")?;

        let reader = RedisCompensationIdempotencyStore::connect(&backend_url).await?;
        let trace_id = format!("redis-compensation-marker-ack-{}", uuid::Uuid::new_v4().simple());
        let circuit = "RedisWireCompensationMarkerAckLossFixture";

        let (proxy_url, mut dropped_reply, proxy) =
            start_proxy_for_command(backend_address, "SET", 1).await?;
        let marker_store = RedisCompensationIdempotencyStore::connect(&proxy_url).await?;
        let effects = Arc::new(AtomicUsize::new(0));
        let compensation_calls = Arc::new(AtomicUsize::new(0));
        let axon = Axon::<u32, u32, String, ()>::new(circuit).then(CountFault(effects.clone()));
        let mut bus = Bus::new();
        let trace_store = InMemoryPersistenceStore::new();
        bus.insert(PersistenceTraceId::new(trace_id.clone()));
        bus.insert(PersistenceHandle::from_store(trace_store.clone()));
        bus.insert(CompensationHandle::from_hook(CountCompensation(
            compensation_calls.clone(),
        )));
        bus.insert(CompensationIdempotencyHandle::from_store(marker_store));

        let report = axon.execute(7, &(), &mut bus).await;
        let observed_drop = timeout(Duration::from_secs(5), dropped_reply.recv())
            .await?
            .context("Redis TCP proxy did not drop the compensation marker SET NX reply")?;
        ensure!(
            observed_drop.command.eq_ignore_ascii_case("SET")
                && observed_drop.ordinal == 1,
            "proxy dropped a different Redis command"
        );
        ensure!(
            observed_drop.response == b"+OK\r\n",
            "Redis did not apply a new SET NX marker"
        );
        ensure!(
            matches!(
                &report,
                ExecutionReport::Unconfirmed {
                    result: Outcome::Fault(message),
                    operation: PersistenceOperation::CompensationWrite,
                    effect: EffectBoundary::MayHaveRun,
                    ..
                } if message == "business-fault"
            ),
            "unexpected compensation marker report: {report:?}"
        );
        let cleanup = report
            .cleanup()
            .context("external compensation cleanup must be reported")?;
        ensure!(cleanup.trace_id == trace_id, "cleanup trace ID changed");
        ensure!(!cleanup.is_complete(), "cleanup was reported complete");
        ensure!(cleanup.steps.len() == 1, "cleanup step count changed");
        ensure!(
            cleanup.steps[0].outcome == ranvier_runtime::SagaRollbackOutcome::Fault,
            "cleanup lost the original business Fault"
        );
        ensure!(effects.load(Ordering::SeqCst) == 1, "domain effect count changed");
        ensure!(
            compensation_calls.load(Ordering::SeqCst) == 1,
            "compensation hook count changed"
        );
        proxy.stop().await?;

        let key = v2_journaled_marker_key(
            &trace_id,
            circuit,
            compensation_intent_sequence(&trace_store, &trace_id).await?,
        );
        ensure!(
            reader.was_compensated(&key).await?,
            "marker applied by Redis disappeared after its acknowledgement was lost"
        );
        println!(
            "REDIS_TCP_COMPENSATION_MARKER_ACK_LOST dropped=SET-NX-1 effect=MayHaveRun effects=1 hook_calls=1 marker_present=true cleanup=Fault trace_store=InMemory redis_trace_store=false"
        );
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("isolated Redis TCP compensation-marker ACK-loss fixture must finish within 20 seconds")??;
    Ok(())
}

#[tokio::test]
#[ignore = "requires explicitly isolated unauthenticated Redis fixture URL and address"]
async fn redis_compensation_marker_read_ack_loss_over_tcp_fails_closed() -> Result<()> {
    use ranvier_runtime::persistence::{
        CompensationHandle, CompensationHook, CompensationIdempotencyHandle,
        CompensationIdempotencyStore, InMemoryPersistenceStore, PersistenceHandle,
        PersistenceTraceId, RedisCompensationIdempotencyStore,
    };

    #[derive(Clone)]
    struct CountFault(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl Transition<u32, u32> for CountFault {
        type Error = String;
        type Resources = ();
        async fn run(&self, _: u32, _: &(), _: &mut Bus) -> Outcome<u32, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Outcome::Fault("business-fault".into())
        }
    }

    #[derive(Clone)]
    struct CountCompensation(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl CompensationHook for CountCompensation {
        async fn compensate(
            &self,
            _: ranvier_runtime::persistence::CompensationContext,
        ) -> anyhow::Result<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    timeout(Duration::from_secs(20), async {
        ensure!(
            std::env::var("RANVIER_ATOMICITY_REDIS_ISOLATED").as_deref() == Ok("1"),
            "set RANVIER_ATOMICITY_REDIS_ISOLATED=1 only for a disposable isolated Redis fixture"
        );
        let backend_url = std::env::var("RANVIER_ATOMICITY_REDIS_URL")
            .context("explicit isolated Redis URL required")?;
        ensure!(
            backend_url.starts_with("redis://") && !backend_url.contains('@'),
            "fixture supports only an isolated unauthenticated redis:// endpoint"
        );
        let redis_url = redis::parse_redis_url(&backend_url)
            .context("fixture Redis URL must be a valid redis:// URL")?;
        ensure!(redis_url.scheme() == "redis", "fixture must not use a TLS or Unix URL");
        let redis_host = redis_url
            .host_str()
            .context("fixture Redis URL must contain a host")?;
        let redis_port = redis_url.port().unwrap_or(6379);
        let backend_endpoint = std::env::var("RANVIER_ATOMICITY_REDIS_ADDRESS")
            .context("explicit isolated Redis address required")?;
        let url_addresses = lookup_host((redis_host, redis_port))
            .await?
            .collect::<std::collections::HashSet<_>>();
        let backend_addresses = lookup_host(&backend_endpoint)
            .await?
            .collect::<std::collections::HashSet<_>>();
        ensure!(
            !backend_addresses.is_empty() && backend_addresses == url_addresses,
            "Redis URL and proxy backend must resolve to the same fixture addresses"
        );
        ensure!(
            backend_addresses.iter().all(|address| match address.ip() {
                IpAddr::V4(ip) => ip.is_private() || ip.is_loopback(),
                IpAddr::V6(ip) => ip.is_unique_local() || ip.is_loopback(),
            }),
            "fixture Redis addresses must be private or loopback"
        );
        let backend_address = *backend_addresses
            .iter()
            .next()
            .context("isolated Redis address did not resolve")?;

        let reader = RedisCompensationIdempotencyStore::connect(&backend_url).await?;
        let trace_id = format!(
            "redis-compensation-read-ack-{}",
            uuid::Uuid::new_v4().simple()
        );
        let circuit = "RedisWireCompensationMarkerReadAckLossFixture";

        let (proxy_url, mut dropped_reply, proxy) =
            start_proxy_for_command(backend_address, "EXISTS", 1).await?;
        let marker_store = RedisCompensationIdempotencyStore::connect(&proxy_url).await?;
        let trace_store = InMemoryPersistenceStore::new();
        let trace_reader = trace_store.clone();
        let effects = Arc::new(AtomicUsize::new(0));
        let compensation_calls = Arc::new(AtomicUsize::new(0));
        let axon = Axon::<u32, u32, String, ()>::new(circuit).then(CountFault(effects.clone()));
        let mut bus = Bus::new();
        bus.insert(PersistenceTraceId::new(trace_id.clone()));
        bus.insert(PersistenceHandle::from_store(trace_store));
        bus.insert(CompensationHandle::from_hook(CountCompensation(
            compensation_calls.clone(),
        )));
        bus.insert(CompensationIdempotencyHandle::from_store(marker_store));

        let report = axon.execute(7, &(), &mut bus).await;
        let observed_drop = timeout(Duration::from_secs(5), dropped_reply.recv())
            .await?
            .context("Redis TCP proxy did not drop the compensation marker EXISTS reply")?;
        ensure!(
            observed_drop.command.eq_ignore_ascii_case("EXISTS")
                && observed_drop.ordinal == 1,
            "proxy dropped a different Redis command"
        );
        ensure!(
            observed_drop.response == b":0\r\n",
            "Redis did not report the new marker as absent"
        );
        ensure!(
            matches!(
                &report,
                ExecutionReport::Unconfirmed {
                    result: Outcome::Fault(message),
                    operation: PersistenceOperation::CompensationRead,
                    effect: EffectBoundary::MayHaveRun,
                    ..
                } if message == "business-fault"
            ),
            "unexpected compensation marker read report: {report:?}"
        );
        let cleanup = report
            .cleanup()
            .context("external compensation cleanup must be reported")?;
        ensure!(cleanup.trace_id == trace_id, "cleanup trace ID changed");
        ensure!(!cleanup.is_complete(), "cleanup was reported complete");
        ensure!(cleanup.steps.len() == 1, "cleanup step count changed");
        ensure!(
            cleanup.steps[0].outcome == ranvier_runtime::SagaRollbackOutcome::NotInvoked,
            "compensation hook ran despite the unconfirmed marker read"
        );
        ensure!(effects.load(Ordering::SeqCst) == 1, "domain effect count changed");
        ensure!(
            compensation_calls.load(Ordering::SeqCst) == 0,
            "compensation hook ran despite the unconfirmed marker read"
        );
        proxy.stop().await?;

        let key = v2_journaled_marker_key(
            &trace_id,
            circuit,
            compensation_intent_sequence(&trace_reader, &trace_id).await?,
        );
        ensure!(
            !reader.was_compensated(&key).await?,
            "marker was written after the unconfirmed read"
        );
        let persisted = trace_reader
            .load(&trace_id)
            .await?
            .context("business trace was not persisted")?;
        ensure!(
            persisted.completion.is_none(),
            "trace was marked complete while compensation reconciliation is required"
        );
        ensure!(
            persisted.events.iter().any(|event| event.outcome_kind == "Fault"),
            "business Fault event was lost after compensation read failure"
        );
        let pending_intent = persisted.events.last().context("compensation intent is missing")?;
        ensure!(
            pending_intent.event_type == ranvier_runtime::persistence::PersistenceEventType::Intent
                && pending_intent.node_id.as_deref() == Some("external-compensation"),
            "compensation marker read uncertainty did not preserve its pending intent"
        );
        ensure!(
            !persisted.events.iter().any(|event| {
                event.event_type == ranvier_runtime::persistence::PersistenceEventType::Terminal
            }),
            "trace was marked terminal while compensation reconciliation is required"
        );

        let mut reentry_bus = Bus::new();
        reentry_bus.insert(PersistenceHandle::from_store(trace_reader.clone()));
        reentry_bus.insert(PersistenceTraceId::new(trace_id.clone()));
        let reentry = axon.execute(7, &(), &mut reentry_bus).await;
        ensure!(
            matches!(
                &reentry,
                ExecutionReport::BlockedBeforeEffect {
                    operation: PersistenceOperation::PendingIntent,
                    effect: EffectBoundary::NotInvoked,
                    ..
                }
            ),
            "re-entry did not require reconciliation before replay: {reentry:?}"
        );
        ensure!(effects.load(Ordering::SeqCst) == 1, "re-entry repeated the business effect");
        ensure!(
            compensation_calls.load(Ordering::SeqCst) == 0,
            "re-entry invoked compensation without operator reconciliation"
        );
        println!(
            "REDIS_TCP_COMPENSATION_MARKER_READ_ACK_LOST dropped=EXISTS-1 response=absent report=Unconfirmed/CompensationRead/MayHaveRun effects=1 hook_calls=0 marker_present=false pending-intent=reentry-blocked"
        );
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("isolated Redis TCP compensation-marker read ACK-loss fixture must finish within 20 seconds")??;
    Ok(())
}
