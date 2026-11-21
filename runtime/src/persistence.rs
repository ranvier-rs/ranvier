use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use tokio::sync::RwLock;

/// Current persisted execution-trace format. It is independent of the
/// user-authored schematic version stored on each trace and event.
pub const PERSISTENCE_FORMAT_VERSION: u16 = 2;

/// Role of an event in the persisted execution protocol.
///
/// `step` remains the logical schematic step. `event_sequence` provides a
/// unique, ordered identity when several protocol events belong to one step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersistenceEventType {
    Enter,
    Intent,
    Result,
    /// A pure orchestration checkpoint that does not invoke a transition.
    Checkpoint,
    Terminal,
    Compensation,
}

pub(crate) fn persistence_event_type_for(
    outcome_kind: &str,
    node_id: Option<&str>,
) -> PersistenceEventType {
    match outcome_kind {
        "Enter" => PersistenceEventType::Enter,
        "Intent" => PersistenceEventType::Intent,
        "Checkpoint" => PersistenceEventType::Checkpoint,
        "Compensated" | "CompensationIncomplete" | "SagaCompensation" => {
            PersistenceEventType::Compensation
        }
        _ if node_id.is_some() => PersistenceEventType::Result,
        _ => PersistenceEventType::Terminal,
    }
}

/// Fail-closed validation for the new event journal. A dangling intent means
/// the associated effect may have run and must be reconciled before resuming.
pub(crate) fn validate_persisted_trace(trace: &PersistedTrace) -> Result<(), TraceValidationError> {
    if trace.format_version != PERSISTENCE_FORMAT_VERSION {
        return Err(TraceValidationError::UnsupportedFormat);
    }

    validate_event_sequence(&trace.events, true)?;
    if trace.completion.is_some()
        && !trace
            .events
            .iter()
            .any(|event| event.event_type == PersistenceEventType::Terminal)
    {
        return Err(TraceValidationError::MissingTerminal);
    }
    Ok(())
}

fn ensure_trace_lifecycle_safe(trace: &PersistedTrace, operation: &str) -> Result<()> {
    validate_persisted_trace(trace).map_err(|error| {
        anyhow!(
            "cannot {operation} trace_id {} with invalid v2 event history: {error:?}",
            trace.trace_id
        )
    })
}

fn ensure_completion_safe(trace: &PersistedTrace, completion: &CompletionState) -> Result<()> {
    let mut candidate = trace.clone();
    candidate.completion = Some(completion.clone());
    ensure_trace_lifecycle_safe(&candidate, "complete")
}

#[cfg(feature = "persistence-postgres")]
fn ensure_terminal_event(events: &[PersistenceEnvelope]) -> Result<()> {
    if events
        .iter()
        .any(|event| event.event_type == PersistenceEventType::Terminal)
    {
        Ok(())
    } else {
        Err(anyhow!(
            "cannot complete persistence trace without a terminal event"
        ))
    }
}

fn completion_already_set(
    existing: Option<&CompletionState>,
    requested: &CompletionState,
    trace_id: &str,
) -> Result<bool> {
    match existing {
        None => Ok(false),
        Some(current) if current == requested => Ok(true),
        Some(_) => Err(conflict(trace_id, PersistenceConflictKind::Completed)),
    }
}

/// Validate an append against the current journal. An Intent is allowed to be
/// the final event; its matching result is required before another intent or a
/// terminal event can be recorded.
pub(crate) fn validate_persistence_append(
    previous: &[PersistenceEnvelope],
    incoming: &PersistenceEnvelope,
) -> Result<()> {
    if previous
        .iter()
        .any(|event| event.event_type == PersistenceEventType::Terminal)
    {
        return Err(conflict(
            &incoming.trace_id,
            PersistenceConflictKind::Terminal,
        ));
    }
    let expected = u64::try_from(previous.len())?;
    if incoming.event_sequence != expected {
        return Err(stale_revision(
            &incoming.trace_id,
            incoming.event_sequence,
            expected,
        ));
    }
    let mut pending = validate_event_sequence(previous, false)
        .map_err(|error| anyhow!("invalid existing persistence event stream: {error:?}"))?;
    apply_event_to_pending(incoming, &mut pending)
        .map_err(|error| anyhow!("invalid persistence event append: {error:?}"))
}

fn validate_event_sequence(
    events: &[PersistenceEnvelope],
    require_settled: bool,
) -> Result<HashSet<(u64, String)>, TraceValidationError> {
    let mut pending_intents = HashSet::new();
    let mut terminal_seen = false;
    for (expected_sequence, event) in events.iter().enumerate() {
        if terminal_seen {
            return Err(TraceValidationError::UnsupportedFormat);
        }
        if event.event_sequence != expected_sequence as u64 {
            return Err(TraceValidationError::UnsupportedFormat);
        }
        apply_event_to_pending(event, &mut pending_intents)?;
        terminal_seen = event.event_type == PersistenceEventType::Terminal;
    }
    if require_settled && !pending_intents.is_empty() {
        return Err(TraceValidationError::PendingIntent);
    }
    Ok(pending_intents)
}

fn apply_event_to_pending(
    event: &PersistenceEnvelope,
    pending_intents: &mut HashSet<(u64, String)>,
) -> Result<(), TraceValidationError> {
    match event.event_type {
        PersistenceEventType::Enter => {
            if !pending_intents.is_empty() {
                return Err(TraceValidationError::PendingIntent);
            }
        }
        PersistenceEventType::Intent => {
            let Some(node_id) = event.node_id.clone() else {
                return Err(TraceValidationError::UnsupportedFormat);
            };
            if !pending_intents.insert((event.step, node_id)) {
                return Err(TraceValidationError::UnsupportedFormat);
            }
        }
        PersistenceEventType::Result | PersistenceEventType::Compensation => {
            let Some(node_id) = event.node_id.as_ref() else {
                return Err(TraceValidationError::UnsupportedFormat);
            };
            if !pending_intents.remove(&(event.step, node_id.clone())) {
                return Err(TraceValidationError::UnsupportedFormat);
            }
        }
        PersistenceEventType::Checkpoint => {
            if event.node_id.is_none() || !pending_intents.is_empty() {
                return Err(if pending_intents.is_empty() {
                    TraceValidationError::UnsupportedFormat
                } else {
                    TraceValidationError::PendingIntent
                });
            }
        }
        PersistenceEventType::Terminal => {
            if event.node_id.is_some() {
                return Err(TraceValidationError::UnsupportedFormat);
            }
            if !pending_intents.is_empty() {
                return Err(TraceValidationError::PendingIntent);
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TraceValidationError {
    UnsupportedFormat,
    PendingIntent,
    MissingTerminal,
}

/// A conditional persistence write that lost to the trace's current state.
///
/// Every write to a trace is valid only on top of the revision (event count)
/// the writer read. Built-in stores return this value inside
/// [`anyhow::Error`] when that revision is stale or the trace no longer
/// accepts the write; recover it with
/// `error.downcast_ref::<PersistenceConflict>()`. The stored trace is
/// unchanged. The runtime never retries a conflicting write: reload the trace
/// and decide again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistenceConflict {
    pub trace_id: String,
    pub kind: PersistenceConflictKind,
}

/// Why a [`PersistenceConflict`] was raised.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PersistenceConflictKind {
    /// The write was based on `expected` events; the trace has `actual`.
    StaleRevision { expected: u64, actual: u64 },
    /// The trace already has its terminal event.
    Terminal,
    /// The trace is completed, or already has a different completion state.
    Completed,
    /// Another intervention is already bound to this revision.
    InterventionExists { revision: u64 },
    /// The stored value changed between read and write (compare-and-set).
    ConcurrentUpdate,
}

impl std::fmt::Display for PersistenceConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let trace_id = &self.trace_id;
        match &self.kind {
            PersistenceConflictKind::StaleRevision { expected, actual } => write!(
                f,
                "trace_id {trace_id} expected event sequence {actual}, got {expected}"
            ),
            PersistenceConflictKind::Terminal => write!(
                f,
                "trace_id {trace_id} cannot append an event after a terminal event"
            ),
            PersistenceConflictKind::Completed => write!(
                f,
                "trace_id {trace_id} is already completed and cannot accept new events or changes"
            ),
            PersistenceConflictKind::InterventionExists { revision } => write!(
                f,
                "trace_id {trace_id} already has an intervention at revision {revision}"
            ),
            PersistenceConflictKind::ConcurrentUpdate => {
                write!(f, "concurrent persistence update for trace_id {trace_id}")
            }
        }
    }
}

impl std::error::Error for PersistenceConflict {}

/// A persistence store whose stored format this build does not support.
///
/// The built-in PostgreSQL and Redis stores record the store format once, in
/// a marker outside the versioned tables or keys, and check it before any
/// trace operation (`ensure_schema()` at startup, otherwise once per store
/// instance). They return this value inside [`anyhow::Error`]; recover it with
/// `error.downcast_ref::<StoreFormatError>()`. Nothing is written when a check
/// fails. Version 1 data is not supported and is never migrated: start on a
/// new or emptied store.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum StoreFormatError {
    /// The store has no format marker and holds version 1 data at `location`
    /// (a table or key).
    LegacyData { location: String },
    /// The format marker holds `found`, not this build's
    /// [`PERSISTENCE_FORMAT_VERSION`]. A later Ranvier version initialized
    /// the store, or the marker was changed by hand.
    UnsupportedVersion { found: String },
    /// The store has no format marker and this operation does not create one.
    /// Call `ensure_schema()` at startup.
    NotInitialized,
}

impl std::fmt::Display for StoreFormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreFormatError::LegacyData { location } => write!(
                f,
                "persistence store holds version 1 data at {location}; version 1 data is not supported: use a new table or key prefix, or remove the version 1 data, then start again"
            ),
            StoreFormatError::UnsupportedVersion { found } => write!(
                f,
                "persistence store format marker is {found:?} but this build supports format {PERSISTENCE_FORMAT_VERSION}; the store was initialized by a different Ranvier version"
            ),
            StoreFormatError::NotInitialized => write!(
                f,
                "persistence store has no format marker; call ensure_schema() at startup before using the store"
            ),
        }
    }
}

impl std::error::Error for StoreFormatError {}

#[cfg(any(feature = "persistence-postgres", feature = "persistence-redis"))]
fn format_marker_matches(found: &str) -> Result<()> {
    if found == PERSISTENCE_FORMAT_VERSION.to_string() {
        Ok(())
    } else {
        Err(StoreFormatError::UnsupportedVersion {
            found: found.to_owned(),
        }
        .into())
    }
}

fn conflict(trace_id: &str, kind: PersistenceConflictKind) -> anyhow::Error {
    anyhow::Error::new(PersistenceConflict {
        trace_id: trace_id.to_owned(),
        kind,
    })
}

fn stale_revision(trace_id: &str, expected: u64, actual: u64) -> anyhow::Error {
    conflict(
        trace_id,
        PersistenceConflictKind::StaleRevision { expected, actual },
    )
}

/// An intervention is accepted only on the revision it was decided on, before
/// the trace ends, and once per revision.
fn ensure_intervention_accepted(trace: &PersistedTrace, intervention: &Intervention) -> Result<()> {
    if trace.completion.is_some() {
        return Err(conflict(
            &trace.trace_id,
            PersistenceConflictKind::Completed,
        ));
    }
    if trace
        .events
        .iter()
        .any(|event| event.event_type == PersistenceEventType::Terminal)
    {
        return Err(conflict(&trace.trace_id, PersistenceConflictKind::Terminal));
    }
    let actual = u64::try_from(trace.events.len())?;
    if intervention.expected_event_sequence != actual {
        return Err(stale_revision(
            &trace.trace_id,
            intervention.expected_event_sequence,
            actual,
        ));
    }
    if trace
        .interventions
        .iter()
        .any(|existing| existing.expected_event_sequence == actual)
    {
        return Err(conflict(
            &trace.trace_id,
            PersistenceConflictKind::InterventionExists { revision: actual },
        ));
    }
    Ok(())
}

/// Minimal persisted envelope for Axon execution checkpoints.
///
/// M148 baseline contract fields:
/// - trace
/// - circuit
/// - step
/// - outcome
/// - timestamp
/// - payload hash
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistenceEnvelope {
    pub trace_id: String,
    pub circuit: String,
    pub schematic_version: String,
    /// Monotonic, zero-based identity within the trace. Unlike `step`, this is
    /// unique when a step has both an intent and a result event.
    #[serde(default)]
    pub event_sequence: u64,
    /// Protocol role, kept separate from the domain `outcome_kind`.
    pub event_type: PersistenceEventType,
    pub step: u64,
    pub node_id: Option<String>,
    pub outcome_kind: String,
    pub timestamp_ms: u64,
    pub payload_hash: Option<String>,
    pub payload: Option<serde_json::Value>,
}

/// Manual intervention command for a trace.
///
/// An intervention is bound to the revision it was decided on:
/// `expected_event_sequence` must equal the trace's event count when it is
/// saved, and the runtime applies it only while the trace is still at that
/// revision. The run that applies it appends events, so it is applied once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intervention {
    pub target_node: String,
    pub payload_override: Option<serde_json::Value>,
    pub timestamp_ms: u64,
    /// Event count of the trace this intervention was decided on.
    pub expected_event_sequence: u64,
}

/// Final completion state tracked for a persisted trace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompletionState {
    Success,
    Fault,
    Cancelled,
    Compensated,
}

#[cfg(feature = "persistence-postgres")]
fn completion_state_to_wire(state: &CompletionState) -> &'static str {
    match state {
        CompletionState::Success => "success",
        CompletionState::Fault => "fault",
        CompletionState::Cancelled => "cancelled",
        CompletionState::Compensated => "compensated",
    }
}

#[cfg(feature = "persistence-postgres")]
fn completion_state_from_wire(value: &str) -> Result<CompletionState> {
    match value {
        "success" => Ok(CompletionState::Success),
        "fault" => Ok(CompletionState::Fault),
        "cancelled" => Ok(CompletionState::Cancelled),
        "compensated" => Ok(CompletionState::Compensated),
        other => Err(anyhow!("unknown completion state value: {}", other)),
    }
}

#[cfg(feature = "persistence-postgres")]
fn persistence_event_type_to_db(event_type: PersistenceEventType) -> &'static str {
    match event_type {
        PersistenceEventType::Enter => "enter",
        PersistenceEventType::Intent => "intent",
        PersistenceEventType::Result => "result",
        PersistenceEventType::Checkpoint => "checkpoint",
        PersistenceEventType::Terminal => "terminal",
        PersistenceEventType::Compensation => "compensation",
    }
}

#[cfg(feature = "persistence-postgres")]
fn persistence_event_type_from_db(value: &str) -> Result<PersistenceEventType> {
    match value {
        "enter" => Ok(PersistenceEventType::Enter),
        "intent" => Ok(PersistenceEventType::Intent),
        "result" => Ok(PersistenceEventType::Result),
        "checkpoint" => Ok(PersistenceEventType::Checkpoint),
        "terminal" => Ok(PersistenceEventType::Terminal),
        "compensation" => Ok(PersistenceEventType::Compensation),
        other => Err(anyhow!("unknown persistence event type value: {other}")),
    }
}

/// Stored trace state returned from [`PersistenceStore::load`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedTrace {
    pub trace_id: String,
    pub circuit: String,
    /// Storage format version, not the user-authored schematic version.
    pub format_version: u16,
    /// The schematic version of the last appended event.
    pub schematic_version: String,
    pub events: Vec<PersistenceEnvelope>,
    pub interventions: Vec<Intervention>,
    pub resumed_from_step: Option<u64>,
    pub completion: Option<CompletionState>,
}

impl PersistedTrace {
    /// The intervention bound to the trace's current revision, if any.
    pub(crate) fn pending_intervention(&self) -> Option<&Intervention> {
        let revision = u64::try_from(self.events.len()).ok()?;
        self.interventions
            .iter()
            .find(|intervention| intervention.expected_event_sequence == revision)
    }

    /// Classify this trace for reconciliation.
    ///
    /// Uses the same rules the runtime applies before executing a trace. The
    /// result only describes the stored history; it does not change it and it
    /// does not say whether an external effect ran. See the persistence
    /// operations runbook for the procedure for each state.
    pub fn reconciliation(&self) -> TraceReconciliation {
        if self.format_version != PERSISTENCE_FORMAT_VERSION {
            return TraceReconciliation::Invalid {
                reason: format!(
                    "unsupported persistence format version {}",
                    self.format_version
                ),
            };
        }
        let pending = match validate_event_sequence(&self.events, false) {
            Ok(pending) => pending,
            Err(error) => {
                return TraceReconciliation::Invalid {
                    reason: format!("invalid event history: {error:?}"),
                };
            }
        };
        let terminal = self
            .events
            .iter()
            .any(|event| event.event_type == PersistenceEventType::Terminal);
        if let Some(completion) = &self.completion {
            return if terminal {
                TraceReconciliation::Completed(completion.clone())
            } else {
                TraceReconciliation::CompletedWithoutTerminal
            };
        }
        if let Some(intent) = self.events.iter().rev().find(|event| {
            event.event_type == PersistenceEventType::Intent
                && event
                    .node_id
                    .as_ref()
                    .is_some_and(|node| pending.contains(&(event.step, node.clone())))
        }) {
            return TraceReconciliation::PendingIntent {
                event_sequence: intent.event_sequence,
                step: intent.step,
                node_id: intent.node_id.clone(),
            };
        }
        if terminal {
            TraceReconciliation::TerminalWithoutCompletion
        } else {
            TraceReconciliation::Open
        }
    }
}

/// Reconciliation state of a stored trace, from
/// [`PersistedTrace::reconciliation`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TraceReconciliation {
    /// No open intent and no terminal event: an execution may run or resume.
    Open,
    /// An effect intent has no result. The effect may have run; confirm it in
    /// the external system before writing a result. The newest open intent is
    /// reported.
    PendingIntent {
        event_sequence: u64,
        step: u64,
        node_id: Option<String>,
    },
    /// The terminal event is recorded but completion is not, for example
    /// after a lost completion acknowledgement.
    TerminalWithoutCompletion,
    /// The trace is complete.
    Completed(CompletionState),
    /// Completion is recorded without a terminal event; the runtime refuses
    /// to execute it.
    CompletedWithoutTerminal,
    /// The history does not follow the current format; the runtime refuses
    /// to execute it.
    Invalid { reason: String },
}

/// Resume cursor returned from [`PersistenceStore::resume`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeCursor {
    pub trace_id: String,
    pub next_step: u64,
}

/// Optional trace identifier override for persistence hooks.
///
/// Insert into `Bus` when a stable trace identity is required across process restarts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistenceTraceId(pub String);

impl PersistenceTraceId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

/// Controls whether runtime execution should call `complete` automatically.
///
/// Default runtime behavior when this resource is absent: `true`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersistenceAutoComplete(pub bool);

/// Runtime context delivered to compensation hooks.
///
/// The context is intentionally compact so hooks can map it to idempotent
/// compensating actions in domain/infrastructure layers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompensationContext {
    pub trace_id: String,
    pub circuit: String,
    pub fault_kind: String,
    pub fault_step: u64,
    pub timestamp_ms: u64,
    /// `event_sequence` of the acknowledged `external-compensation` intent in
    /// the execution trace store, or `None` when no trace store is configured.
    ///
    /// The value is stable for one compensation effect: the intent is written
    /// before the hook runs and a later invocation of the same trace stops at
    /// the open intent instead of creating a new one. Hooks can pass it,
    /// together with `trace_id` and `circuit`, to a provider as an
    /// idempotency key.
    #[serde(default)]
    pub intent_sequence: Option<u64>,
}

/// Marker key shared by automatic compensation and report-bound readback.
///
/// Version 2 framing: every variable field is preceded by its UTF-8 byte
/// length, so distinct contexts cannot produce the same key.
///
/// - journaled (trace store configured): `v2:j:<intent_sequence>:<trace>:<circuit>`
/// - unjournaled (no trace store): `v2:u:<fault_step>:<fault_kind>:<trace>:<circuit>`
///
/// where each text field is written as `<len>:<value>`. Version 1 keys
/// (`trace:circuit:fault_kind`) are neither read nor written. A missing
/// marker never authorizes a retry by itself.
pub(crate) fn compensation_idempotency_key(context: &CompensationContext) -> String {
    fn push_field(key: &mut String, value: &str) {
        key.push(':');
        key.push_str(&value.len().to_string());
        key.push(':');
        key.push_str(value);
    }
    let mut key = String::from("v2");
    match context.intent_sequence {
        Some(sequence) => {
            key.push_str(":j:");
            key.push_str(&sequence.to_string());
        }
        None => {
            key.push_str(":u:");
            key.push_str(&context.fault_step.to_string());
            push_field(&mut key, &context.fault_kind);
        }
    }
    push_field(&mut key, &context.trace_id);
    push_field(&mut key, &context.circuit);
    key
}

/// Controls whether compensation hooks should run automatically on `Fault`.
///
/// Default runtime behavior when this resource is absent: `true`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompensationAutoTrigger(pub bool);

/// Retry policy for compensation hook execution.
///
/// Defaults to a single attempt (no retry).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompensationRetryPolicy {
    pub max_attempts: u32,
    pub backoff_ms: u64,
}

impl Default for CompensationRetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 1,
            backoff_ms: 0,
        }
    }
}

/// Upper bound on the time one execution spends in cleanup.
///
/// Insert into the Bus before execution. Cleanup is Saga rollback and external
/// compensation after a fault, and the cleanup that follows cancellation. The
/// window starts when cleanup first starts and covers every cleanup step of
/// the invocation, including compensation retries and their backoff.
///
/// When the window closes, a running handler is dropped and reported as
/// `SagaRollbackOutcome::InProgress`; handlers not yet started are
/// `NotInvoked`. The compensation intent stays open, no terminal event or
/// completion is written, and the execution is reported `Unconfirmed` for
/// reconciliation. The budget bounds how long cleanup runs; it does not cancel
/// an external effect. Without this resource cleanup is unbounded. Streaming
/// supervision uses the same window with its own grace period.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CleanupBudget(pub std::time::Duration);

/// Compensation hook contract for irreversible side effects.
///
/// **Production-stable since 0.12 (M156).** Implement and register this hook
/// via [`CompensationHandle`] to define rollback actions when an Axon execution
/// faults after committing external side effects. Registration is independent
/// of the execution-trace store: the hook still runs without one, while the
/// returned [`crate::ExecutionReport`] remains unpersisted unless that separate
/// trace store confirms its event writes. Marker-store uncertainty is reported
/// independently from execution-trace durability.
#[async_trait]
pub trait CompensationHook: Send + Sync {
    async fn compensate(&self, context: CompensationContext) -> Result<()>;
}

/// Bus-insertable compensation hook handle used by runtime execution hooks.
#[derive(Clone)]
pub struct CompensationHandle {
    inner: Arc<dyn CompensationHook>,
}

impl std::fmt::Debug for CompensationHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompensationHandle").finish_non_exhaustive()
    }
}

impl CompensationHandle {
    /// Create a handle from a concrete compensation hook implementation.
    pub fn from_hook<H>(hook: H) -> Self
    where
        H: CompensationHook + 'static,
    {
        Self {
            inner: Arc::new(hook),
        }
    }

    /// Create a handle from an existing trait-object Arc.
    pub fn from_arc(hook: Arc<dyn CompensationHook>) -> Self {
        Self { inner: hook }
    }

    /// Access the shared compensation hook.
    pub fn hook(&self) -> Arc<dyn CompensationHook> {
        self.inner.clone()
    }
}

/// Idempotency store contract for compensation execution deduplication.
///
/// Tracks completed compensation keys across executions. The separate
/// read, external hook, and marker write are not an atomic claim protocol.
/// Lost acknowledgements and concurrent callers require reconciliation or
/// downstream deduplication before retry; this trait alone cannot guarantee
/// at-most-once external actions across failures or process restarts.
///
/// Keys are opaque runtime strings (see `CompensationContext::intent_sequence`
/// for the identity they encode). When an execution trace store is
/// configured, an expired or purged marker does not by itself re-enable the
/// hook: the trace's open compensation intent or its `Compensated` event
/// still stops a later invocation for reconciliation.
///
/// Built-in adapters: [`InMemoryCompensationIdempotencyStore`],
/// [`PostgresCompensationIdempotencyStore`] (feature `persistence-postgres`),
/// [`RedisCompensationIdempotencyStore`] (feature `persistence-redis`).
#[async_trait]
pub trait CompensationIdempotencyStore: Send + Sync {
    async fn was_compensated(&self, key: &str) -> Result<bool>;
    async fn mark_compensated(&self, key: &str) -> Result<()>;
}

/// Bus-insertable idempotency handle for compensation hooks.
#[derive(Clone)]
pub struct CompensationIdempotencyHandle {
    inner: Arc<dyn CompensationIdempotencyStore>,
    observation: Option<Arc<crate::execution_observation::Observation>>,
}

impl std::fmt::Debug for CompensationIdempotencyHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompensationIdempotencyHandle")
            .finish_non_exhaustive()
    }
}

impl CompensationIdempotencyHandle {
    pub fn from_store<S>(store: S) -> Self
    where
        S: CompensationIdempotencyStore + 'static,
    {
        Self {
            inner: Arc::new(store),
            observation: None,
        }
    }

    pub fn from_arc(store: Arc<dyn CompensationIdempotencyStore>) -> Self {
        Self {
            inner: store,
            observation: None,
        }
    }

    #[cfg(all(test, feature = "streaming"))]
    pub(crate) fn shares_store_with(&self, store: &Arc<dyn CompensationIdempotencyStore>) -> bool {
        Arc::ptr_eq(&self.inner, store)
    }

    pub fn store(&self) -> Arc<dyn CompensationIdempotencyStore> {
        match &self.observation {
            None => self.inner.clone(),
            Some(observation) => {
                Arc::new(crate::execution_observation::ObservedCompensationStore {
                    inner: self.inner.clone(),
                    observation: observation.clone(),
                })
            }
        }
    }

    pub(crate) fn observed(
        &self,
        observation: Arc<crate::execution_observation::Observation>,
    ) -> Self {
        Self {
            inner: self.inner.clone(),
            observation: Some(observation),
        }
    }
}

/// In-memory idempotency store for compensation deduplication.
#[derive(Debug, Default, Clone)]
pub struct InMemoryCompensationIdempotencyStore {
    keys: Arc<RwLock<HashSet<String>>>,
}

impl InMemoryCompensationIdempotencyStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl CompensationIdempotencyStore for InMemoryCompensationIdempotencyStore {
    async fn was_compensated(&self, key: &str) -> Result<bool> {
        let guard = self.keys.read().await;
        Ok(guard.contains(key))
    }

    async fn mark_compensated(&self, key: &str) -> Result<()> {
        let mut guard = self.keys.write().await;
        guard.insert(key.to_string());
        Ok(())
    }
}

#[cfg(feature = "persistence-postgres")]
#[derive(Debug, Clone)]
struct PostgresRelation {
    sql: std::result::Result<String, &'static str>,
}

#[cfg(feature = "persistence-postgres")]
impl PostgresRelation {
    fn from_prefix(prefix: &str, suffix: &str) -> Self {
        Self {
            sql: postgres_relation_sql(prefix, suffix),
        }
    }

    fn sql(&self) -> Result<&str> {
        self.sql
            .as_deref()
            .map_err(|reason| anyhow!("invalid PostgreSQL table prefix: {reason}"))
    }
}

#[cfg(feature = "persistence-postgres")]
fn reject_postgres_relation_group_if_any_is_invalid(relations: &mut [PostgresRelation]) {
    let reason = relations
        .iter()
        .find_map(|relation| relation.sql.as_ref().err().copied());
    if let Some(reason) = reason {
        for relation in relations {
            relation.sql = Err(reason);
        }
    }
}

#[cfg(feature = "persistence-postgres")]
#[derive(Debug)]
enum PostgresIdentifierPart {
    Unquoted(String),
    Quoted(String),
}

#[cfg(feature = "persistence-postgres")]
const POSTGRES_DEFAULT_MAX_IDENTIFIER_BYTES: usize = 63;

#[cfg(feature = "persistence-postgres")]
fn postgres_relation_sql(prefix: &str, suffix: &str) -> std::result::Result<String, &'static str> {
    let characters = prefix.chars().collect::<Vec<_>>();
    let mut cursor = 0;
    let mut parts = Vec::new();
    skip_postgres_identifier_whitespace(&characters, &mut cursor);

    if cursor == characters.len() {
        return Ok(suffix.to_string());
    }

    loop {
        if cursor >= characters.len() {
            return Err("missing identifier component");
        }

        let part = if characters[cursor] == '"' {
            cursor += 1;
            let mut value = String::new();
            let mut closed = false;

            while cursor < characters.len() {
                let character = characters[cursor];
                if character == '\0' {
                    return Err("NUL is not allowed in an identifier");
                }
                if character == '"' {
                    if characters.get(cursor + 1) == Some(&'"') {
                        value.push('"');
                        cursor += 2;
                    } else {
                        cursor += 1;
                        closed = true;
                        break;
                    }
                } else {
                    value.push(character);
                    cursor += 1;
                }
            }

            if !closed || value.is_empty() {
                return Err("quoted identifier is empty or unterminated");
            }
            PostgresIdentifierPart::Quoted(value)
        } else {
            let first = characters[cursor];
            if !is_postgres_unquoted_identifier_start(first) {
                return Err("expected a PostgreSQL identifier component");
            }

            let mut value = String::new();
            value.push(first);
            cursor += 1;
            while cursor < characters.len()
                && is_postgres_unquoted_identifier_continue(characters[cursor])
            {
                value.push(characters[cursor]);
                cursor += 1;
            }
            PostgresIdentifierPart::Unquoted(value)
        };
        parts.push(part);

        skip_postgres_identifier_whitespace(&characters, &mut cursor);
        if cursor == characters.len() {
            break;
        }
        if characters[cursor] != '.' {
            return Err("only dot-separated identifiers are allowed");
        }

        cursor += 1;
        skip_postgres_identifier_whitespace(&characters, &mut cursor);
        if cursor == characters.len() {
            // A trailing dot was valid with the old suffix concatenation: `schema.`
            // selected `schema._events` (or the matching relation suffix).
            parts.push(PostgresIdentifierPart::Unquoted(String::new()));
            break;
        }
    }

    if parts.len() > 3 {
        return Err("at most three qualified identifier components are allowed");
    }

    let Some(last) = parts.last_mut() else {
        return Err("missing identifier component");
    };
    match last {
        PostgresIdentifierPart::Unquoted(value) | PostgresIdentifierPart::Quoted(value) => {
            value.push_str(suffix);
        }
    }

    if parts.iter().any(|part| {
        let value = match part {
            PostgresIdentifierPart::Unquoted(value) | PostgresIdentifierPart::Quoted(value) => {
                value
            }
        };
        value.len() > POSTGRES_DEFAULT_MAX_IDENTIFIER_BYTES
    }) {
        return Err("identifier component exceeds PostgreSQL's 63-byte default limit");
    }

    Ok(parts
        .into_iter()
        .map(|part| match part {
            // Keep unquoted components unquoted so PostgreSQL applies exactly its normal
            // case-folding rules, including database-specific folding of non-ASCII names.
            PostgresIdentifierPart::Unquoted(value) => value,
            PostgresIdentifierPart::Quoted(value) => {
                format!("\"{}\"", value.replace('"', "\"\""))
            }
        })
        .collect::<Vec<_>>()
        .join("."))
}

#[cfg(feature = "persistence-postgres")]
fn skip_postgres_identifier_whitespace(characters: &[char], cursor: &mut usize) {
    while characters
        .get(*cursor)
        .is_some_and(|character| character.is_ascii_whitespace())
    {
        *cursor += 1;
    }
}

#[cfg(feature = "persistence-postgres")]
fn is_postgres_unquoted_identifier_start(character: char) -> bool {
    character == '_'
        || character.is_ascii_alphabetic()
        || (!character.is_ascii() && !character.is_control())
}

#[cfg(feature = "persistence-postgres")]
fn is_postgres_unquoted_identifier_continue(character: char) -> bool {
    is_postgres_unquoted_identifier_start(character)
        || character.is_ascii_digit()
        || character == '$'
}

#[cfg(feature = "persistence-postgres")]
#[derive(Debug, Clone)]
pub struct PostgresCompensationIdempotencyStore {
    pool: sqlx::Pool<sqlx::Postgres>,
    table: PostgresRelation,
}

#[cfg(feature = "persistence-postgres")]
impl PostgresCompensationIdempotencyStore {
    /// Create a PostgreSQL-backed compensation idempotency store.
    pub fn new(pool: sqlx::Pool<sqlx::Postgres>) -> Self {
        Self::with_table_prefix(pool, "ranvier_persistence")
    }

    /// Create with a custom PostgreSQL relation prefix.
    ///
    /// The prefix must be a dot-separated identifier path. Unquoted components
    /// start with an ASCII letter, underscore, or non-control Unicode code point
    /// and may continue with those characters, digits, or `$`; quoted components
    /// use SQL double quotes with doubled quotes for escaping. At most three
    /// qualified components are accepted, and every completed identifier must fit
    /// PostgreSQL's default 63-byte limit after the relation suffix is appended.
    /// Invalid prefixes are rejected by the first store operation. Treat the prefix
    /// as trusted configuration: selecting a relation does not authorize access to
    /// a tenant.
    pub fn with_table_prefix(pool: sqlx::Pool<sqlx::Postgres>, prefix: impl Into<String>) -> Self {
        let prefix = prefix.into();
        Self {
            pool,
            table: PostgresRelation::from_prefix(&prefix, "_compensation_idempotency"),
        }
    }

    /// Initialize adapter table when absent.
    pub async fn ensure_schema(&self) -> Result<()> {
        let create = format!(
            "CREATE TABLE IF NOT EXISTS {} (
                idempotency_key TEXT PRIMARY KEY,
                created_at_ms BIGINT NOT NULL
            )",
            self.table.sql()?
        );
        sqlx::query(&create).execute(&self.pool).await?;
        Ok(())
    }

    /// Remove stale idempotency rows older than `cutoff_ms` (unix epoch ms).
    pub async fn purge_older_than_ms(&self, cutoff_ms: i64) -> Result<u64> {
        let query = format!(
            "DELETE FROM {}
             WHERE created_at_ms < $1",
            self.table.sql()?
        );
        let rows = sqlx::query(&query)
            .bind(cutoff_ms)
            .execute(&self.pool)
            .await?
            .rows_affected();
        Ok(rows)
    }
}

#[cfg(feature = "persistence-postgres")]
#[async_trait]
impl CompensationIdempotencyStore for PostgresCompensationIdempotencyStore {
    async fn was_compensated(&self, key: &str) -> Result<bool> {
        let query = format!(
            "SELECT 1
             FROM {}
             WHERE idempotency_key = $1
             LIMIT 1",
            self.table.sql()?
        );
        let row: Option<i32> = sqlx::query_scalar(&query)
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some())
    }

    async fn mark_compensated(&self, key: &str) -> Result<()> {
        let query = format!(
            "INSERT INTO {} (idempotency_key, created_at_ms)
             VALUES ($1, $2)
             ON CONFLICT (idempotency_key) DO NOTHING",
            self.table.sql()?
        );
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis();
        sqlx::query(&query)
            .bind(key)
            .bind(i64::try_from(now_ms)?)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[cfg(feature = "persistence-redis")]
#[derive(Clone)]
pub struct RedisCompensationIdempotencyStore {
    manager: redis::aio::ConnectionManager,
    key_prefix: String,
    ttl_seconds: Option<u64>,
}

#[cfg(feature = "persistence-redis")]
impl std::fmt::Debug for RedisCompensationIdempotencyStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisCompensationIdempotencyStore")
            .field("key_prefix", &self.key_prefix)
            .field("ttl_seconds", &self.ttl_seconds)
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "persistence-redis")]
impl RedisCompensationIdempotencyStore {
    /// Connect using Redis connection URL.
    pub async fn connect(url: &str) -> Result<Self> {
        let client = redis::Client::open(url)?;
        let manager = redis::aio::ConnectionManager::new(client).await?;
        Ok(Self {
            manager,
            key_prefix: "ranvier:compensation:idempotency".to_string(),
            ttl_seconds: None,
        })
    }

    pub fn with_prefix(
        manager: redis::aio::ConnectionManager,
        key_prefix: impl Into<String>,
    ) -> Self {
        Self {
            manager,
            key_prefix: key_prefix.into(),
            ttl_seconds: None,
        }
    }

    pub fn with_prefix_and_ttl(
        manager: redis::aio::ConnectionManager,
        key_prefix: impl Into<String>,
        ttl_seconds: u64,
    ) -> Self {
        Self {
            manager,
            key_prefix: key_prefix.into(),
            ttl_seconds: Some(ttl_seconds),
        }
    }

    fn key(&self, idempotency_key: &str) -> String {
        format!("{}:{}", self.key_prefix, idempotency_key)
    }
}

#[cfg(feature = "persistence-redis")]
#[async_trait]
impl CompensationIdempotencyStore for RedisCompensationIdempotencyStore {
    async fn was_compensated(&self, key: &str) -> Result<bool> {
        use redis::AsyncCommands;
        let mut conn = self.manager.clone();
        let exists: bool = conn.exists(self.key(key)).await?;
        Ok(exists)
    }

    async fn mark_compensated(&self, key: &str) -> Result<()> {
        let mut conn = self.manager.clone();
        let redis_key = self.key(key);
        // One command, so a marker can never be left without its configured TTL.
        let mut command = redis::cmd("SET");
        command.arg(&redis_key).arg("1").arg("NX");
        if let Some(ttl_seconds) = self.ttl_seconds {
            command.arg("EX").arg(ttl_seconds);
        }
        let _: Option<String> = command.query_async(&mut conn).await?;
        Ok(())
    }
}

/// Persistence abstraction for long-running workflow crash recovery.
///
/// **Production-stable since 0.12 (M156).**
///
/// Implement this trait to store and reload Axon execution checkpoints across
/// process restarts. The runtime calls `append` on each step and `complete` on
/// terminal outcomes. Use `resume` to replay a pipeline from a saved cursor.
///
/// # Built-in adapters
///
/// | Adapter | Feature flag | Best for |
/// |---|---|---|
/// | [`InMemoryPersistenceStore`] | none (default) | tests, local dev |
/// | [`PostgresPersistenceStore`] | `persistence-postgres` | durable prod storage |
/// | [`RedisPersistenceStore`] | `persistence-redis` | ephemeral/fast checkpoints |
///
/// # Concurrent writers
///
/// Several processes may execute the same trace. Stores must make every write
/// conditional on the trace's revision (its event count) and apply it
/// atomically:
///
/// - `append` succeeds only when `event_sequence` equals the current event
///   count, the trace has no terminal event, and it is not completed.
/// - `complete` requires a terminal event and never replaces a different
///   completion state.
/// - `save_intervention` succeeds only when
///   [`Intervention::expected_event_sequence`] equals the current event count,
///   the trace has not ended, and no other intervention is bound to that
///   revision.
///
/// A losing write leaves the trace unchanged and should return a
/// [`PersistenceConflict`]. The built-in PostgreSQL and Redis (single node)
/// adapters provide this across processes; [`InMemoryPersistenceStore`]
/// provides it within one process. Custom stores are not verified by the
/// runtime. Effects that already ran are not fenced: a writer that loses after
/// its effect reports the outcome as unconfirmed.
///
/// # See also
///
/// - `docs/03_guides/persistence_ops_runbook.md` — operational playbook
/// - `docs/manual/04_PERSISTENCE.md` — concept guide + adapter selection
#[async_trait]
pub trait PersistenceStore: Send + Sync {
    async fn append(&self, envelope: PersistenceEnvelope) -> Result<()>;
    async fn load(&self, trace_id: &str) -> Result<Option<PersistedTrace>>;
    async fn resume(&self, trace_id: &str, resume_from_step: u64) -> Result<ResumeCursor>;
    async fn complete(&self, trace_id: &str, completion: CompletionState) -> Result<()>;
    async fn save_intervention(&self, trace_id: &str, intervention: Intervention) -> Result<()>;
}

/// Bus-insertable persistence handle used by runtime execution hooks.
#[derive(Clone)]
pub struct PersistenceHandle {
    inner: Arc<dyn PersistenceStore>,
    observation: Option<Arc<crate::execution_observation::Observation>>,
    next_event_sequence: Arc<AtomicU64>,
}

impl std::fmt::Debug for PersistenceHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersistenceHandle").finish_non_exhaustive()
    }
}

impl PersistenceHandle {
    /// Create a handle from a concrete store implementation.
    pub fn from_store<S>(store: S) -> Self
    where
        S: PersistenceStore + 'static,
    {
        Self {
            inner: Arc::new(store),
            observation: None,
            next_event_sequence: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Create a handle from an existing trait-object Arc.
    pub fn from_arc(store: Arc<dyn PersistenceStore>) -> Self {
        Self {
            inner: store,
            observation: None,
            next_event_sequence: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Access the shared persistence store.
    pub fn store(&self) -> Arc<dyn PersistenceStore> {
        match &self.observation {
            Some(observation) => Arc::new(crate::execution_observation::ObservedStore {
                inner: self.inner.clone(),
                observation: observation.clone(),
            }),
            None => self.inner.clone(),
        }
    }
    pub(crate) fn observed(
        &self,
        observation: Arc<crate::execution_observation::Observation>,
    ) -> Self {
        Self {
            inner: self.inner.clone(),
            observation: Some(observation),
            next_event_sequence: Arc::new(AtomicU64::new(0)),
        }
    }

    pub(crate) fn allocate_event_sequence(&self) -> u64 {
        self.next_event_sequence.fetch_add(1, Ordering::SeqCst)
    }

    pub(crate) fn set_next_event_sequence(&self, sequence: u64) {
        self.next_event_sequence.store(sequence, Ordering::SeqCst);
    }
}

/// In-memory reference adapter for local testing and contract validation.
#[derive(Debug, Default, Clone)]
pub struct InMemoryPersistenceStore {
    inner: Arc<RwLock<HashMap<String, PersistedTrace>>>,
}

impl InMemoryPersistenceStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl PersistenceStore for InMemoryPersistenceStore {
    async fn append(&self, envelope: PersistenceEnvelope) -> Result<()> {
        let mut guard = self.inner.write().await;
        if !guard.contains_key(&envelope.trace_id) {
            if envelope.event_sequence != 0 {
                return Err(stale_revision(
                    &envelope.trace_id,
                    envelope.event_sequence,
                    0,
                ));
            }
            validate_persistence_append(&[], &envelope)?;
            guard.insert(
                envelope.trace_id.clone(),
                PersistedTrace {
                    trace_id: envelope.trace_id.clone(),
                    circuit: envelope.circuit.clone(),
                    format_version: PERSISTENCE_FORMAT_VERSION,
                    schematic_version: envelope.schematic_version.clone(),
                    events: vec![envelope],
                    interventions: Vec::new(),
                    resumed_from_step: None,
                    completion: None,
                },
            );
            return Ok(());
        }
        let entry = guard.get_mut(&envelope.trace_id).expect("checked above");

        if entry.circuit != envelope.circuit {
            return Err(anyhow!(
                "trace_id {} already exists for circuit {}, got {}",
                envelope.trace_id,
                entry.circuit,
                envelope.circuit
            ));
        }
        if entry.completion.is_some() {
            return Err(conflict(
                &envelope.trace_id,
                PersistenceConflictKind::Completed,
            ));
        }
        if entry.format_version != PERSISTENCE_FORMAT_VERSION {
            return Err(anyhow!(
                "trace_id {} uses unsupported persistence format {}",
                envelope.trace_id,
                entry.format_version
            ));
        }
        let expected_sequence = u64::try_from(entry.events.len())?;
        if envelope.event_sequence != expected_sequence {
            return Err(stale_revision(
                &envelope.trace_id,
                envelope.event_sequence,
                expected_sequence,
            ));
        }
        validate_persistence_append(&entry.events, &envelope)?;
        entry.schematic_version = envelope.schematic_version.clone();
        entry.events.push(envelope);
        entry.events.sort_by_key(|e| e.event_sequence);
        Ok(())
    }

    async fn load(&self, trace_id: &str) -> Result<Option<PersistedTrace>> {
        let guard = self.inner.read().await;
        Ok(guard.get(trace_id).cloned())
    }

    async fn resume(&self, trace_id: &str, resume_from_step: u64) -> Result<ResumeCursor> {
        let mut guard = self.inner.write().await;
        let trace = guard
            .get_mut(trace_id)
            .ok_or_else(|| anyhow!("trace_id {} not found", trace_id))?;
        if trace.completion.is_some() {
            return Err(conflict(trace_id, PersistenceConflictKind::Completed));
        }
        ensure_trace_lifecycle_safe(trace, "resume")?;
        trace.resumed_from_step = Some(resume_from_step);
        Ok(ResumeCursor {
            trace_id: trace_id.to_string(),
            next_step: resume_from_step.saturating_add(1),
        })
    }

    async fn complete(&self, trace_id: &str, completion: CompletionState) -> Result<()> {
        let mut guard = self.inner.write().await;
        let trace = guard
            .get_mut(trace_id)
            .ok_or_else(|| anyhow!("trace_id {} not found", trace_id))?;
        ensure_trace_lifecycle_safe(trace, "complete")?;
        if completion_already_set(trace.completion.as_ref(), &completion, trace_id)? {
            return Ok(());
        }
        ensure_completion_safe(trace, &completion)?;
        trace.completion = Some(completion);
        Ok(())
    }

    async fn save_intervention(&self, trace_id: &str, intervention: Intervention) -> Result<()> {
        let mut guard = self.inner.write().await;
        let trace = guard
            .get_mut(trace_id)
            .ok_or_else(|| anyhow!("trace_id {} not found", trace_id))?;
        ensure_intervention_accepted(trace, &intervention)?;
        trace.interventions.push(intervention);
        Ok(())
    }
}

#[cfg(feature = "persistence-postgres")]
#[derive(Debug, Clone)]
pub struct PostgresPersistenceStore {
    pool: sqlx::Pool<sqlx::Postgres>,
    events_table: PostgresRelation,
    state_table: PostgresRelation,
    interventions_table: PostgresRelation,
    legacy_events_table: PostgresRelation,
    legacy_state_table: PostgresRelation,
    legacy_interventions_table: PostgresRelation,
    format_table: PostgresRelation,
    format_ready: Arc<tokio::sync::OnceCell<()>>,
}

#[cfg(feature = "persistence-postgres")]
#[derive(sqlx::FromRow)]
struct PostgresEventRow {
    trace_id: String,
    circuit: String,
    schematic_version: String,
    event_sequence: i64,
    event_type: String,
    step: i64,
    node_id: Option<String>,
    outcome_kind: String,
    timestamp_ms: i64,
    payload_hash: Option<String>,
    payload: Option<serde_json::Value>,
}

#[cfg(feature = "persistence-postgres")]
#[derive(sqlx::FromRow)]
struct PostgresStateRow {
    trace_id: String,
    circuit: String,
    schematic_version: String,
    format_version: i32,
    resumed_from_step: Option<i64>,
    completion: Option<String>,
}

#[cfg(feature = "persistence-postgres")]
#[derive(sqlx::FromRow)]
struct PostgresInterventionRow {
    #[sqlx(rename = "trace_id")]
    _trace_id: String,
    target_node: String,
    payload_override: Option<serde_json::Value>,
    timestamp_ms: i64,
    expected_event_sequence: i64,
}

#[cfg(feature = "persistence-postgres")]
impl PostgresPersistenceStore {
    /// Create a PostgreSQL-backed persistence store.
    ///
    /// Uses the default table prefix `ranvier_persistence`. Call
    /// `ensure_schema()` once at startup to create required tables.
    /// See [`Self::with_table_prefix`] for multi-tenant setups.
    pub fn new(pool: sqlx::Pool<sqlx::Postgres>) -> Self {
        Self::with_table_prefix(pool, "ranvier_persistence")
    }

    /// Create with a custom PostgreSQL relation prefix.
    ///
    /// The prefix must be a dot-separated identifier path. Unquoted components
    /// start with an ASCII letter, underscore, or non-control Unicode code point
    /// and may continue with those characters, digits, or `$`; quoted components
    /// use SQL double quotes with doubled quotes for escaping. At most three
    /// qualified components are accepted, and every relation used by this store
    /// must fit PostgreSQL's default 63-byte identifier limit after its suffix is
    /// appended. Invalid prefixes are rejected by the first store operation. Treat
    /// the prefix as trusted configuration: selecting a relation does not authorize
    /// access to a tenant.
    pub fn with_table_prefix(pool: sqlx::Pool<sqlx::Postgres>, prefix: impl Into<String>) -> Self {
        let prefix = prefix.into();
        let mut relations = [
            PostgresRelation::from_prefix(&prefix, "_v2_events"),
            PostgresRelation::from_prefix(&prefix, "_v2_state"),
            PostgresRelation::from_prefix(&prefix, "_v2_interventions"),
            PostgresRelation::from_prefix(&prefix, "_events"),
            PostgresRelation::from_prefix(&prefix, "_state"),
            PostgresRelation::from_prefix(&prefix, "_interventions"),
            PostgresRelation::from_prefix(&prefix, "_format"),
        ];
        reject_postgres_relation_group_if_any_is_invalid(&mut relations);
        let [
            events_table,
            state_table,
            interventions_table,
            legacy_events_table,
            legacy_state_table,
            legacy_interventions_table,
            format_table,
        ] = relations;
        Self {
            pool,
            events_table,
            state_table,
            interventions_table,
            legacy_events_table,
            legacy_state_table,
            legacy_interventions_table,
            format_table,
            format_ready: Arc::new(tokio::sync::OnceCell::new()),
        }
    }

    async fn relation_exists(conn: &mut sqlx::PgConnection, relation: &str) -> Result<bool> {
        Ok(
            sqlx::query_scalar::<_, bool>("SELECT to_regclass($1) IS NOT NULL")
                .bind(relation)
                .fetch_one(&mut *conn)
                .await?,
        )
    }

    async fn read_format(
        conn: &mut sqlx::PgConnection,
        format_table: &str,
    ) -> Result<Option<String>> {
        if !Self::relation_exists(conn, format_table).await? {
            return Ok(None);
        }
        let query = format!("SELECT format_version::text FROM {format_table} WHERE id = 1");
        Ok(sqlx::query_scalar::<_, Option<String>>(&query)
            .fetch_optional(&mut *conn)
            .await?
            .map(Option::unwrap_or_default))
    }

    /// Check the store format marker and the version 1 tables. With
    /// `initialize`, create the marker on a store that has none.
    async fn check_format(&self, conn: &mut sqlx::PgConnection, initialize: bool) -> Result<()> {
        let format_table = self.format_table.sql()?;
        let marker = Self::read_format(conn, format_table).await?;
        if let Some(found) = &marker {
            format_marker_matches(found)?;
        }
        // Checked even when the marker matches: a downgraded v0.51.x binary
        // writes version 1 tables and ignores the marker. The check costs
        // three catalog lookups.
        for relation in [
            &self.legacy_state_table,
            &self.legacy_events_table,
            &self.legacy_interventions_table,
        ] {
            let relation = relation.sql()?;
            if Self::relation_exists(conn, relation).await? {
                let query = format!("SELECT EXISTS (SELECT 1 FROM {relation})");
                if sqlx::query_scalar::<_, bool>(&query)
                    .fetch_one(&mut *conn)
                    .await?
                {
                    return Err(StoreFormatError::LegacyData {
                        location: relation.to_owned(),
                    }
                    .into());
                }
            }
        }
        if marker.is_some() {
            return Ok(());
        }
        if !initialize {
            return Err(StoreFormatError::NotInitialized.into());
        }
        let create = format!(
            "CREATE TABLE IF NOT EXISTS {format_table} (
                id SMALLINT PRIMARY KEY CHECK (id = 1),
                format_version INTEGER NOT NULL
            )"
        );
        sqlx::query(&create).execute(&mut *conn).await?;
        let insert = format!(
            "INSERT INTO {format_table} (id, format_version) VALUES (1, $1) ON CONFLICT (id) DO NOTHING"
        );
        sqlx::query(&insert)
            .bind(i32::from(PERSISTENCE_FORMAT_VERSION))
            .execute(&mut *conn)
            .await?;
        match Self::read_format(conn, format_table).await? {
            Some(found) => format_marker_matches(&found),
            None => Err(StoreFormatError::NotInitialized.into()),
        }
    }

    /// Run the format check once per store instance (shared by clones).
    async fn format_ready(&self) -> Result<()> {
        // Reject an invalid prefix before using the pool.
        self.format_table.sql()?;
        self.format_ready
            .get_or_try_init(|| async {
                let mut conn = self.pool.acquire().await?;
                self.check_format(&mut conn, false).await
            })
            .await?;
        Ok(())
    }

    /// Check the store format and create the adapter tables when absent.
    ///
    /// Call once at startup. Refuses, without writing anything, a store whose
    /// format marker names another version or whose version 1 tables hold
    /// rows ([`StoreFormatError`]); the version 1 tables are checked on every
    /// call. On a store without a marker it records this build's format. Concurrent calls are serialized with a
    /// transaction-scoped advisory lock. Existing tables are not altered or
    /// migrated.
    pub async fn ensure_schema(&self) -> Result<()> {
        let state_table = self.state_table.sql()?;
        let events_table = self.events_table.sql()?;
        let interventions_table = self.interventions_table.sql()?;
        let format_table = self.format_table.sql()?;
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
            .bind(format_table)
            .execute(&mut *tx)
            .await?;
        self.check_format(&mut tx, true).await?;
        let create_state = format!(
            "CREATE TABLE IF NOT EXISTS {} (
                trace_id TEXT PRIMARY KEY,
                circuit TEXT NOT NULL,
                schematic_version TEXT NOT NULL,
                format_version INTEGER NOT NULL,
                resumed_from_step BIGINT NULL,
                completion TEXT NULL
            )",
            state_table
        );
        sqlx::query(&create_state).execute(&mut *tx).await?;

        let create_events = format!(
            "CREATE TABLE IF NOT EXISTS {} (
                trace_id TEXT NOT NULL,
                circuit TEXT NOT NULL,
                schematic_version TEXT NOT NULL,
                event_sequence BIGINT NOT NULL,
                event_type TEXT NOT NULL,
                step BIGINT NOT NULL,
                node_id TEXT NULL,
                outcome_kind TEXT NOT NULL,
                timestamp_ms BIGINT NOT NULL,
                payload_hash TEXT NULL,
                payload JSONB NULL,
                PRIMARY KEY (trace_id, event_sequence)
            )",
            events_table
        );
        sqlx::query(&create_events).execute(&mut *tx).await?;

        let create_interventions = format!(
            "CREATE TABLE IF NOT EXISTS {} (
                trace_id TEXT NOT NULL,
                target_node TEXT NOT NULL,
                payload_override JSONB NULL,
                timestamp_ms BIGINT NOT NULL,
                expected_event_sequence BIGINT NOT NULL,
                FOREIGN KEY (trace_id) REFERENCES {} (trace_id),
                UNIQUE (trace_id, expected_event_sequence)
            )",
            interventions_table, state_table
        );
        sqlx::query(&create_interventions).execute(&mut *tx).await?;
        // `CREATE TABLE IF NOT EXISTS` keeps a table created by an earlier
        // build. Such tables are not migrated (no prior data is supported);
        // refuse them here instead of failing on the first intervention.
        let probe = format!(
            "SELECT expected_event_sequence FROM {} LIMIT 0",
            interventions_table
        );
        sqlx::query(&probe).execute(&mut *tx).await.map_err(|error| {
            anyhow!(
                "interventions table {interventions_table} predates revision-bound interventions; recreate the persistence tables: {error}"
            )
        })?;

        tx.commit().await?;
        let _ = self.format_ready.set(());
        Ok(())
    }
}

#[cfg(feature = "persistence-postgres")]
fn postgres_event_sequence_is_contiguous(count: i64, max_sequence: Option<i64>) -> bool {
    match (count, max_sequence) {
        (0, None) => true,
        (count, Some(max_sequence)) if count > 0 => max_sequence.checked_add(1) == Some(count),
        _ => false,
    }
}

#[cfg(feature = "persistence-postgres")]
#[async_trait]
impl PersistenceStore for PostgresPersistenceStore {
    async fn append(&self, envelope: PersistenceEnvelope) -> Result<()> {
        self.format_ready().await?;
        // Validate representability before creating any persistent state.
        let event_sequence_i64 = i64::try_from(envelope.event_sequence)?;
        let step_i64 = i64::try_from(envelope.step)?;
        let ts_i64 = i64::try_from(envelope.timestamp_ms)?;
        let state_table = self.state_table.sql()?;
        let events_table = self.events_table.sql()?;
        let mut transaction = self.pool.begin().await?;
        let insert_state = format!(
            "INSERT INTO {} (trace_id, circuit, schematic_version, format_version, resumed_from_step, completion)
             VALUES ($1, $2, $3, $4, NULL, NULL)
             ON CONFLICT (trace_id) DO NOTHING",
            state_table
        );
        sqlx::query(&insert_state)
            .bind(&envelope.trace_id)
            .bind(&envelope.circuit)
            .bind(&envelope.schematic_version)
            .bind(i32::from(PERSISTENCE_FORMAT_VERSION))
            .execute(&mut *transaction)
            .await?;
        let read_state = format!(
            "SELECT circuit, format_version, completion FROM {} WHERE trace_id = $1 FOR UPDATE",
            state_table
        );
        let (circuit, format_version, completion): (String, i32, Option<String>) =
            sqlx::query_as(&read_state)
                .bind(&envelope.trace_id)
                .fetch_one(&mut *transaction)
                .await?;
        if circuit != envelope.circuit {
            return Err(anyhow!(
                "trace_id {} already exists for another circuit",
                envelope.trace_id
            ));
        }
        if completion.is_some() {
            return Err(conflict(
                &envelope.trace_id,
                PersistenceConflictKind::Completed,
            ));
        }
        if format_version != i32::from(PERSISTENCE_FORMAT_VERSION) {
            return Err(anyhow!(
                "trace_id {} has unsupported v2 namespace format {}",
                envelope.trace_id,
                format_version
            ));
        }
        let event_state: (i64, Option<i64>) = sqlx::query_as(&format!(
            "SELECT COUNT(*)::BIGINT, MAX(event_sequence) FROM {} WHERE trace_id = $1",
            events_table
        ))
        .bind(&envelope.trace_id)
        .fetch_one(&mut *transaction)
        .await?;
        if !postgres_event_sequence_is_contiguous(event_state.0, event_state.1) {
            return Err(anyhow!(
                "trace_id {} has a non-contiguous v2 event sequence",
                envelope.trace_id
            ));
        }
        let expected_sequence = u64::try_from(event_state.0)?;
        if envelope.event_sequence != expected_sequence {
            return Err(stale_revision(
                &envelope.trace_id,
                envelope.event_sequence,
                expected_sequence,
            ));
        }
        let existing_query = format!(
            "SELECT trace_id, circuit, schematic_version, event_sequence, event_type, step, node_id, outcome_kind, timestamp_ms, payload_hash, payload
             FROM {}
             WHERE trace_id = $1
             ORDER BY event_sequence ASC",
            events_table
        );
        let rows: Vec<PostgresEventRow> = sqlx::query_as(&existing_query)
            .bind(&envelope.trace_id)
            .fetch_all(&mut *transaction)
            .await?;
        let mut previous = Vec::with_capacity(rows.len());
        for row in rows {
            previous.push(PersistenceEnvelope {
                trace_id: row.trace_id,
                circuit: row.circuit,
                schematic_version: row.schematic_version,
                event_sequence: u64::try_from(row.event_sequence)?,
                event_type: persistence_event_type_from_db(&row.event_type)?,
                step: u64::try_from(row.step)?,
                node_id: row.node_id,
                outcome_kind: row.outcome_kind,
                timestamp_ms: u64::try_from(row.timestamp_ms)?,
                payload_hash: row.payload_hash,
                payload: row.payload,
            });
        }
        validate_persistence_append(&previous, &envelope)?;
        let update_state = format!(
            "UPDATE {} SET schematic_version = $2 WHERE trace_id = $1",
            state_table
        );
        sqlx::query(&update_state)
            .bind(&envelope.trace_id)
            .bind(&envelope.schematic_version)
            .execute(&mut *transaction)
            .await?;
        let insert_event = format!(
            "INSERT INTO {} (trace_id, circuit, schematic_version, event_sequence, event_type, step, node_id, outcome_kind, timestamp_ms, payload_hash, payload)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
            events_table
        );
        sqlx::query(&insert_event)
            .bind(&envelope.trace_id)
            .bind(&envelope.circuit)
            .bind(&envelope.schematic_version)
            .bind(event_sequence_i64)
            .bind(persistence_event_type_to_db(envelope.event_type))
            .bind(step_i64)
            .bind(&envelope.node_id)
            .bind(&envelope.outcome_kind)
            .bind(ts_i64)
            .bind(&envelope.payload_hash)
            .bind(&envelope.payload)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn load(&self, trace_id: &str) -> Result<Option<PersistedTrace>> {
        self.format_ready().await?;
        // State, events and interventions must represent the same MVCC snapshot.
        let state_table = self.state_table.sql()?;
        let events_table = self.events_table.sql()?;
        let interventions_table = self.interventions_table.sql()?;
        let mut transaction = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *transaction)
            .await?;
        let state_query = format!(
            "SELECT trace_id, circuit, schematic_version, format_version, resumed_from_step, completion
             FROM {}
             WHERE trace_id = $1",
            state_table
        );
        let Some(state): Option<PostgresStateRow> = sqlx::query_as(&state_query)
            .bind(trace_id)
            .fetch_optional(&mut *transaction)
            .await?
        else {
            transaction.commit().await?;
            return Ok(None);
        };
        if state.format_version != i32::from(PERSISTENCE_FORMAT_VERSION) {
            return Err(anyhow!(
                "trace_id {trace_id} has unsupported persistence format {}",
                state.format_version
            ));
        }

        let events_query = format!(
            "SELECT trace_id, circuit, schematic_version, event_sequence, event_type, step, node_id, outcome_kind, timestamp_ms, payload_hash, payload
             FROM {}
             WHERE trace_id = $1
             ORDER BY event_sequence ASC",
            events_table
        );
        let rows: Vec<PostgresEventRow> = sqlx::query_as(&events_query)
            .bind(trace_id)
            .fetch_all(&mut *transaction)
            .await?;

        let mut events = Vec::with_capacity(rows.len());
        for row in rows {
            events.push(PersistenceEnvelope {
                trace_id: row.trace_id,
                circuit: row.circuit,
                schematic_version: row.schematic_version,
                event_sequence: u64::try_from(row.event_sequence)?,
                event_type: persistence_event_type_from_db(&row.event_type)?,
                step: u64::try_from(row.step)?,
                node_id: row.node_id,
                outcome_kind: row.outcome_kind,
                timestamp_ms: u64::try_from(row.timestamp_ms)?,
                payload_hash: row.payload_hash,
                payload: row.payload,
            });
        }

        let completion = match state.completion {
            Some(value) => Some(completion_state_from_wire(&value)?),
            None => None,
        };

        let interventions_query = format!(
            "SELECT trace_id, target_node, payload_override, timestamp_ms, expected_event_sequence
             FROM {}
             WHERE trace_id = $1
             ORDER BY expected_event_sequence ASC",
            interventions_table
        );
        let intervention_rows: Vec<PostgresInterventionRow> = sqlx::query_as(&interventions_query)
            .bind(trace_id)
            .fetch_all(&mut *transaction)
            .await?;

        let mut interventions = Vec::with_capacity(intervention_rows.len());
        for row in intervention_rows {
            interventions.push(Intervention {
                target_node: row.target_node,
                payload_override: row.payload_override,
                timestamp_ms: u64::try_from(row.timestamp_ms)?,
                expected_event_sequence: u64::try_from(row.expected_event_sequence)?,
            });
        }

        let trace = PersistedTrace {
            trace_id: state.trace_id,
            circuit: state.circuit,
            format_version: u16::try_from(state.format_version)?,
            schematic_version: state.schematic_version,
            events,
            interventions,
            resumed_from_step: state.resumed_from_step.map(u64::try_from).transpose()?,
            completion,
        };
        transaction.commit().await?;
        Ok(Some(trace))
    }

    async fn resume(&self, trace_id: &str, resume_from_step: u64) -> Result<ResumeCursor> {
        self.format_ready().await?;
        let state_table = self.state_table.sql()?;
        let events_table = self.events_table.sql()?;
        let mut transaction = self.pool.begin().await?;
        let state_query = format!(
            "SELECT format_version, completion FROM {} WHERE trace_id = $1 FOR UPDATE",
            state_table
        );
        let Some((format_version, completion)): Option<(i32, Option<String>)> =
            sqlx::query_as(&state_query)
                .bind(trace_id)
                .fetch_optional(&mut *transaction)
                .await?
        else {
            return Err(anyhow!("trace_id {} not found", trace_id));
        };
        if format_version != i32::from(PERSISTENCE_FORMAT_VERSION) {
            return Err(anyhow!(
                "trace_id {trace_id} has unsupported persistence format {format_version}"
            ));
        }
        if completion.is_some() {
            return Err(conflict(trace_id, PersistenceConflictKind::Completed));
        }
        let events_query = format!(
            "SELECT trace_id, circuit, schematic_version, event_sequence, event_type, step, node_id, outcome_kind, timestamp_ms, payload_hash, payload
             FROM {} WHERE trace_id = $1 ORDER BY event_sequence ASC",
            events_table
        );
        let rows: Vec<PostgresEventRow> = sqlx::query_as(&events_query)
            .bind(trace_id)
            .fetch_all(&mut *transaction)
            .await?;
        let mut events = Vec::with_capacity(rows.len());
        for row in rows {
            events.push(PersistenceEnvelope {
                trace_id: row.trace_id,
                circuit: row.circuit,
                schematic_version: row.schematic_version,
                event_sequence: u64::try_from(row.event_sequence)?,
                event_type: persistence_event_type_from_db(&row.event_type)?,
                step: u64::try_from(row.step)?,
                node_id: row.node_id,
                outcome_kind: row.outcome_kind,
                timestamp_ms: u64::try_from(row.timestamp_ms)?,
                payload_hash: row.payload_hash,
                payload: row.payload,
            });
        }
        validate_event_sequence(&events, true).map_err(|error| {
            anyhow!("cannot resume trace_id {trace_id} with invalid v2 event history: {error:?}")
        })?;
        let update_query = format!(
            "UPDATE {} SET resumed_from_step = $2 WHERE trace_id = $1 AND completion IS NULL",
            state_table
        );
        let rows = sqlx::query(&update_query)
            .bind(trace_id)
            .bind(i64::try_from(resume_from_step)?)
            .execute(&mut *transaction)
            .await?
            .rows_affected();
        if rows != 1 {
            return Err(anyhow!("trace_id {} could not be resumed", trace_id));
        }
        transaction.commit().await?;
        Ok(ResumeCursor {
            trace_id: trace_id.to_string(),
            next_step: resume_from_step.saturating_add(1),
        })
    }

    async fn complete(&self, trace_id: &str, completion: CompletionState) -> Result<()> {
        self.format_ready().await?;
        let state_table = self.state_table.sql()?;
        let events_table = self.events_table.sql()?;
        let mut transaction = self.pool.begin().await?;
        let state_query = format!(
            "SELECT format_version, completion FROM {} WHERE trace_id = $1 FOR UPDATE",
            state_table
        );
        let Some((format_version, current_completion)): Option<(i32, Option<String>)> =
            sqlx::query_as(&state_query)
                .bind(trace_id)
                .fetch_optional(&mut *transaction)
                .await?
        else {
            return Err(anyhow!("trace_id {} not found", trace_id));
        };
        if format_version != i32::from(PERSISTENCE_FORMAT_VERSION) {
            return Err(anyhow!(
                "trace_id {trace_id} has unsupported persistence format {format_version}"
            ));
        }
        let current_completion = current_completion
            .as_deref()
            .map(completion_state_from_wire)
            .transpose()?;
        let events_query = format!(
            "SELECT trace_id, circuit, schematic_version, event_sequence, event_type, step, node_id, outcome_kind, timestamp_ms, payload_hash, payload
             FROM {} WHERE trace_id = $1 ORDER BY event_sequence ASC",
            events_table
        );
        let rows: Vec<PostgresEventRow> = sqlx::query_as(&events_query)
            .bind(trace_id)
            .fetch_all(&mut *transaction)
            .await?;
        let mut events = Vec::with_capacity(rows.len());
        for row in rows {
            events.push(PersistenceEnvelope {
                trace_id: row.trace_id,
                circuit: row.circuit,
                schematic_version: row.schematic_version,
                event_sequence: u64::try_from(row.event_sequence)?,
                event_type: persistence_event_type_from_db(&row.event_type)?,
                step: u64::try_from(row.step)?,
                node_id: row.node_id,
                outcome_kind: row.outcome_kind,
                timestamp_ms: u64::try_from(row.timestamp_ms)?,
                payload_hash: row.payload_hash,
                payload: row.payload,
            });
        }
        validate_event_sequence(&events, true).map_err(|error| {
            anyhow!("cannot complete trace_id {trace_id} with invalid v2 event history: {error:?}")
        })?;
        ensure_terminal_event(&events)?;
        if completion_already_set(current_completion.as_ref(), &completion, trace_id)? {
            transaction.commit().await?;
            return Ok(());
        }
        let update_query = format!(
            "UPDATE {} SET completion = $2 WHERE trace_id = $1",
            state_table
        );
        let rows = sqlx::query(&update_query)
            .bind(trace_id)
            .bind(completion_state_to_wire(&completion))
            .execute(&mut *transaction)
            .await?
            .rows_affected();
        if rows == 0 {
            return Err(anyhow!("trace_id {} not found", trace_id));
        }
        transaction.commit().await?;
        Ok(())
    }

    async fn save_intervention(&self, trace_id: &str, intervention: Intervention) -> Result<()> {
        self.format_ready().await?;
        let ts_i64 = i64::try_from(intervention.timestamp_ms)?;
        let revision_i64 = i64::try_from(intervention.expected_event_sequence)?;
        let state_table = self.state_table.sql()?;
        let events_table = self.events_table.sql()?;
        let interventions_table = self.interventions_table.sql()?;
        // The state row lock serializes this check with appends and completion.
        let mut transaction = self.pool.begin().await?;
        let state_query = format!(
            "SELECT format_version, completion FROM {} WHERE trace_id = $1 FOR UPDATE",
            state_table
        );
        let Some((format_version, completion)): Option<(i32, Option<String>)> =
            sqlx::query_as(&state_query)
                .bind(trace_id)
                .fetch_optional(&mut *transaction)
                .await?
        else {
            return Err(anyhow!("trace_id {} not found", trace_id));
        };
        if format_version != i32::from(PERSISTENCE_FORMAT_VERSION) {
            return Err(anyhow!(
                "trace_id {trace_id} has unsupported persistence format {format_version}"
            ));
        }
        if completion.is_some() {
            return Err(conflict(trace_id, PersistenceConflictKind::Completed));
        }
        let (count, terminal): (i64, bool) = sqlx::query_as(&format!(
            "SELECT COUNT(*)::BIGINT, COALESCE(BOOL_OR(event_type = 'terminal'), FALSE)
             FROM {} WHERE trace_id = $1",
            events_table
        ))
        .bind(trace_id)
        .fetch_one(&mut *transaction)
        .await?;
        if terminal {
            return Err(conflict(trace_id, PersistenceConflictKind::Terminal));
        }
        let actual = u64::try_from(count)?;
        if intervention.expected_event_sequence != actual {
            return Err(stale_revision(
                trace_id,
                intervention.expected_event_sequence,
                actual,
            ));
        }
        let exists: bool = sqlx::query_scalar(&format!(
            "SELECT EXISTS (SELECT 1 FROM {} WHERE trace_id = $1 AND expected_event_sequence = $2)",
            interventions_table
        ))
        .bind(trace_id)
        .bind(revision_i64)
        .fetch_one(&mut *transaction)
        .await?;
        if exists {
            return Err(conflict(
                trace_id,
                PersistenceConflictKind::InterventionExists { revision: actual },
            ));
        }
        let query = format!(
            "INSERT INTO {} (trace_id, target_node, payload_override, timestamp_ms, expected_event_sequence)
             VALUES ($1, $2, $3, $4, $5)",
            interventions_table
        );
        sqlx::query(&query)
            .bind(trace_id)
            .bind(&intervention.target_node)
            .bind(&intervention.payload_override)
            .bind(ts_i64)
            .bind(revision_i64)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(())
    }
}

#[cfg(all(test, feature = "persistence-redis"))]
struct RedisWriteGate {
    ready: tokio::sync::Semaphore,
    release: tokio::sync::Semaphore,
    expected: tokio::sync::Mutex<Option<String>>,
}

#[cfg(feature = "persistence-redis")]
fn escape_redis_glob(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(character, '*' | '?' | '[' | ']' | '\\' | '^' | '-') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

#[cfg(feature = "persistence-redis")]
#[derive(Clone)]
pub struct RedisPersistenceStore {
    manager: redis::aio::ConnectionManager,
    key_prefix: String,
    format_ready: Arc<tokio::sync::OnceCell<()>>,
    #[cfg(test)]
    write_gate: Option<std::sync::Arc<RedisWriteGate>>,
}

#[cfg(feature = "persistence-redis")]
impl std::fmt::Debug for RedisPersistenceStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisPersistenceStore")
            .field("key_prefix", &self.key_prefix)
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "persistence-redis")]
impl RedisPersistenceStore {
    /// Connect using Redis connection URL.
    ///
    /// Example: `redis://127.0.0.1:6379`
    pub async fn connect(url: &str) -> Result<Self> {
        let client = redis::Client::open(url)?;
        let manager = redis::aio::ConnectionManager::new(client).await?;
        Ok(Self {
            manager,
            key_prefix: "ranvier:persistence".to_string(),
            format_ready: Arc::new(tokio::sync::OnceCell::new()),
            #[cfg(test)]
            write_gate: None,
        })
    }

    pub fn with_prefix(
        manager: redis::aio::ConnectionManager,
        key_prefix: impl Into<String>,
    ) -> Self {
        Self {
            manager,
            key_prefix: key_prefix.into(),
            format_ready: Arc::new(tokio::sync::OnceCell::new()),
            #[cfg(test)]
            write_gate: None,
        }
    }

    fn key(&self, trace_id: &str) -> String {
        format!("{}:v2:{}", self.key_prefix, trace_id)
    }

    fn format_key(&self) -> String {
        format!("{}:format", self.key_prefix)
    }

    /// Check the store format and record it on a store without a marker.
    ///
    /// Call once at startup. Refuses, without writing anything, a store whose
    /// format marker (`{prefix}:format`) names another version or that has no
    /// marker and holds version 1 keys (`{prefix}:{trace_id}`)
    /// ([`StoreFormatError`]). Store operations run the same check once per
    /// store instance when this was not called. Version 1 keys are looked for
    /// only while the store has no marker, because the scan walks the whole
    /// keyspace; keys a downgraded v0.51.x binary writes after that are not
    /// detected and stay invisible to this build.
    pub async fn ensure_schema(&self) -> Result<()> {
        self.check_format().await?;
        let _ = self.format_ready.set(());
        Ok(())
    }

    async fn format_ready(&self) -> Result<()> {
        self.format_ready
            .get_or_try_init(|| self.check_format())
            .await?;
        Ok(())
    }

    async fn check_format(&self) -> Result<()> {
        use redis::AsyncCommands;
        let mut conn = self.manager.clone();
        let format_key = self.format_key();
        let found: Option<String> = conn.get(&format_key).await?;
        if let Some(found) = found {
            // Version 1 keys are looked for only on a store without a marker:
            // the scan walks the whole keyspace, so it is not repeated on
            // every start. Keys a downgraded v0.51.x binary writes later are
            // not detected; they stay invisible to this build.
            return format_marker_matches(&found);
        }
        let v2_prefix = format!("{}:v2:", self.key_prefix);
        let pattern = format!("{}:*", escape_redis_glob(&self.key_prefix));
        let mut cursor: u64 = 0;
        loop {
            let (next, keys): (u64, Vec<Vec<u8>>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(1000)
                .query_async(&mut conn)
                .await?;
            if let Some(legacy) = keys.into_iter().find(|key| {
                !key.starts_with(v2_prefix.as_bytes()) && key.as_slice() != format_key.as_bytes()
            }) {
                return Err(StoreFormatError::LegacyData {
                    location: String::from_utf8_lossy(&legacy).into_owned(),
                }
                .into());
            }
            if next == 0 {
                break;
            }
            cursor = next;
        }
        let _: Option<String> = redis::cmd("SET")
            .arg(&format_key)
            .arg(PERSISTENCE_FORMAT_VERSION.to_string())
            .arg("NX")
            .query_async(&mut conn)
            .await?;
        let found: Option<String> = conn.get(&format_key).await?;
        match found {
            Some(found) => format_marker_matches(&found),
            None => Err(StoreFormatError::NotInitialized.into()),
        }
    }

    async fn read_trace(&self, trace_id: &str) -> Result<(Option<String>, Option<PersistedTrace>)> {
        use redis::AsyncCommands;
        self.format_ready().await?;
        let mut conn = self.manager.clone();
        let payload: Option<String> = conn.get(self.key(trace_id)).await?;
        let trace = payload
            .as_deref()
            .map(serde_json::from_str::<PersistedTrace>)
            .transpose()?;
        if trace
            .as_ref()
            .is_some_and(|value| value.trace_id != trace_id)
        {
            return Err(anyhow!("persisted trace_id does not match requested key"));
        }
        if let Some(trace) = trace.as_ref()
            && trace.format_version != PERSISTENCE_FORMAT_VERSION
        {
            return Err(anyhow!(
                "trace_id {trace_id} has unsupported persistence format {}",
                trace.format_version
            ));
        }
        Ok((payload, trace))
    }

    async fn write_trace(&self, trace: &PersistedTrace, expected: Option<&str>) -> Result<()> {
        let key = self.key(&trace.trace_id);
        let payload = serde_json::to_string(trace)?;
        let mut conn = self.manager.clone();
        #[cfg(test)]
        if let Some(gate) = &self.write_gate {
            *gate.expected.lock().await = expected.map(str::to_owned);
            gate.ready.add_permits(1);
            gate.release.acquire().await?.forget();
        }
        // Compare opaque JSON bytes: Lua must not round u64 payloads via cjson.
        let updated: i64 = redis::cmd("EVAL")
            .arg("local current = redis.call('GET', KEYS[1]); if ARGV[1] == '1' then if current ~= ARGV[2] then return 0 end else if current then return 0 end end; redis.call('SET', KEYS[1], ARGV[3]); return 1")
            .arg(1)
            .arg(key)
            .arg(if expected.is_some() { "1" } else { "0" })
            .arg(expected.unwrap_or_default())
            .arg(payload)
            .query_async(&mut conn)
            .await?;
        if updated != 1 {
            return Err(conflict(
                &trace.trace_id,
                PersistenceConflictKind::ConcurrentUpdate,
            ));
        }
        Ok(())
    }
}

#[cfg(feature = "persistence-redis")]
#[async_trait]
impl PersistenceStore for RedisPersistenceStore {
    async fn append(&self, envelope: PersistenceEnvelope) -> Result<()> {
        let (previous, loaded) = self.read_trace(&envelope.trace_id).await?;
        let mut trace = loaded.unwrap_or_else(|| PersistedTrace {
            trace_id: envelope.trace_id.clone(),
            circuit: envelope.circuit.clone(),
            format_version: PERSISTENCE_FORMAT_VERSION,
            schematic_version: envelope.schematic_version.clone(),
            events: Vec::new(),
            interventions: Vec::new(),
            resumed_from_step: None,
            completion: None,
        });

        if trace.circuit != envelope.circuit {
            return Err(anyhow!(
                "trace_id {} already exists for circuit {}, got {}",
                envelope.trace_id,
                trace.circuit,
                envelope.circuit
            ));
        }
        if trace.completion.is_some() {
            return Err(conflict(
                &envelope.trace_id,
                PersistenceConflictKind::Completed,
            ));
        }

        if trace.format_version != PERSISTENCE_FORMAT_VERSION {
            return Err(anyhow!(
                "trace_id {} has unsupported persistence format {}",
                envelope.trace_id,
                trace.format_version
            ));
        }
        let expected_sequence = u64::try_from(trace.events.len())?;
        if envelope.event_sequence != expected_sequence {
            return Err(stale_revision(
                &envelope.trace_id,
                envelope.event_sequence,
                expected_sequence,
            ));
        }
        validate_persistence_append(&trace.events, &envelope)?;

        trace.schematic_version = envelope.schematic_version.clone();
        trace.events.push(envelope);
        trace.events.sort_by_key(|event| event.event_sequence);
        self.write_trace(&trace, previous.as_deref()).await?;
        Ok(())
    }

    async fn load(&self, trace_id: &str) -> Result<Option<PersistedTrace>> {
        Ok(self.read_trace(trace_id).await?.1)
    }

    async fn resume(&self, trace_id: &str, resume_from_step: u64) -> Result<ResumeCursor> {
        let (previous, loaded) = self.read_trace(trace_id).await?;
        let mut trace = loaded.ok_or_else(|| anyhow!("trace_id {} not found", trace_id))?;
        if trace.completion.is_some() {
            return Err(conflict(trace_id, PersistenceConflictKind::Completed));
        }
        ensure_trace_lifecycle_safe(&trace, "resume")?;
        trace.resumed_from_step = Some(resume_from_step);
        self.write_trace(&trace, previous.as_deref()).await?;
        Ok(ResumeCursor {
            trace_id: trace_id.to_string(),
            next_step: resume_from_step.saturating_add(1),
        })
    }

    async fn complete(&self, trace_id: &str, completion: CompletionState) -> Result<()> {
        let (previous, loaded) = self.read_trace(trace_id).await?;
        let mut trace = loaded.ok_or_else(|| anyhow!("trace_id {} not found", trace_id))?;
        ensure_trace_lifecycle_safe(&trace, "complete")?;
        if completion_already_set(trace.completion.as_ref(), &completion, trace_id)? {
            return Ok(());
        }
        ensure_completion_safe(&trace, &completion)?;
        trace.completion = Some(completion);
        self.write_trace(&trace, previous.as_deref()).await?;
        Ok(())
    }

    async fn save_intervention(&self, trace_id: &str, intervention: Intervention) -> Result<()> {
        let (previous, loaded) = self.read_trace(trace_id).await?;
        let mut trace = loaded.ok_or_else(|| anyhow!("trace_id {} not found", trace_id))?;
        ensure_intervention_accepted(&trace, &intervention)?;
        trace.interventions.push(intervention);
        self.write_trace(&trace, previous.as_deref()).await?;
        Ok(())
    }
}

#[cfg(all(test, feature = "persistence-redis"))]
#[path = "redis_process_fixture.rs"]
mod process_fixture;

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(any(feature = "persistence-postgres", feature = "persistence-redis"))]
    use uuid::Uuid;

    #[cfg(feature = "persistence-postgres")]
    #[test]
    fn postgres_event_sequence_contiguity_accepts_empty_table_and_rejects_gaps() {
        assert!(postgres_event_sequence_is_contiguous(0, None));
        assert!(postgres_event_sequence_is_contiguous(1, Some(0)));
        assert!(postgres_event_sequence_is_contiguous(6, Some(5)));

        assert!(!postgres_event_sequence_is_contiguous(0, Some(0)));
        assert!(!postgres_event_sequence_is_contiguous(1, None));
        assert!(!postgres_event_sequence_is_contiguous(2, Some(0)));
        assert!(!postgres_event_sequence_is_contiguous(2, Some(2)));
        assert!(!postgres_event_sequence_is_contiguous(-1, None));
    }

    #[cfg(feature = "persistence-postgres")]
    #[test]
    fn postgres_relation_prefix_preserves_qualified_identifier_behavior() {
        let cases = [
            ("", "_state"),
            ("tenant_7", "tenant_7_state"),
            ("tenant$1", "tenant$1_state"),
            ("租户_7", "租户_7_state"),
            // PostgreSQL still receives unquoted text and retains its normal case folding.
            ("Tenant_7", "Tenant_7_state"),
            ("tenant_schema.tenant_7", "tenant_schema.tenant_7_state"),
            ("tenant_schema . tenant_7", "tenant_schema.tenant_7_state"),
            ("tenant_schema.", "tenant_schema._state"),
            (
                r#""Tenant Schema"."Tenant 7""#,
                r#""Tenant Schema"."Tenant 7_state""#,
            ),
        ];

        for (prefix, expected) in cases {
            let relation = PostgresRelation::from_prefix(prefix, "_state");
            assert_eq!(relation.sql().unwrap(), expected, "prefix {prefix:?}");
        }
    }

    #[cfg(feature = "persistence-postgres")]
    #[test]
    fn postgres_relation_prefix_rejects_components_postgres_would_truncate() {
        let longest_suffix = "_compensation_idempotency";
        let fitting_prefix =
            "p".repeat(POSTGRES_DEFAULT_MAX_IDENTIFIER_BYTES - longest_suffix.len());
        let fitting_relation = PostgresRelation::from_prefix(&fitting_prefix, longest_suffix);
        assert_eq!(
            fitting_relation.sql().unwrap(),
            format!("{fitting_prefix}{longest_suffix}")
        );

        let oversized_prefix = format!("{fitting_prefix}p");
        assert!(
            PostgresRelation::from_prefix(&oversized_prefix, longest_suffix)
                .sql()
                .is_err()
        );

        // PostgreSQL's identifier limit counts encoded bytes, including for quoted names.
        let multibyte_prefix = format!("{}界", "s".repeat(54));
        assert_eq!(multibyte_prefix.len(), 57);
        assert_eq!(
            PostgresRelation::from_prefix(&multibyte_prefix, "_state")
                .sql()
                .unwrap(),
            format!("{multibyte_prefix}_state")
        );
        let oversized_multibyte = format!("{multibyte_prefix}x");
        assert!(
            PostgresRelation::from_prefix(&oversized_multibyte, "_state")
                .sql()
                .is_err()
        );

        let oversized_schema = format!("{}.tenant", "s".repeat(64));
        assert!(
            PostgresRelation::from_prefix(&oversized_schema, "_state")
                .sql()
                .is_err()
        );

        let quoted_prefix = format!(r#""{}""#, "q".repeat(57));
        assert_eq!(
            PostgresRelation::from_prefix(&quoted_prefix, "_state")
                .sql()
                .unwrap(),
            format!(r#""{}_state""#, "q".repeat(57))
        );
        let oversized_quoted_prefix = format!(r#""{}""#, "q".repeat(58));
        assert!(
            PostgresRelation::from_prefix(&oversized_quoted_prefix, "_state")
                .sql()
                .is_err()
        );
    }

    #[cfg(feature = "persistence-postgres")]
    #[test]
    fn postgres_relation_prefix_quotes_payload_and_rejects_sql_fragments() {
        let quoted_payload =
            PostgresRelation::from_prefix(r#""tenant; DROP TABLE users;--""#, "_state");
        assert_eq!(
            quoted_payload.sql().unwrap(),
            r#""tenant; DROP TABLE users;--_state""#
        );

        let escaped_quote = PostgresRelation::from_prefix(r#""odd""tenant""#, "_state");
        assert_eq!(escaped_quote.sql().unwrap(), r#""odd""tenant_state""#);

        for prefix in [
            "tenant; DROP TABLE users;--",
            "tenant--",
            "tenant/*comment*/",
            "tenant$1; DROP TABLE users;--",
            "tenant.$1",
            "$1",
            "tenant..other",
            ".tenant",
            r#""tenant"; DROP TABLE users;--"#,
            r#""unterminated"#,
            r#"""#,
            "1tenant",
            "one.two.three.four",
        ] {
            let relation = PostgresRelation::from_prefix(prefix, "_state");
            assert!(
                relation.sql().is_err(),
                "prefix {prefix:?} should fail closed"
            );
        }
    }

    #[cfg(feature = "persistence-postgres")]
    #[tokio::test]
    async fn invalid_postgres_relation_prefixes_fail_before_pool_use() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://user:password@127.0.0.1:1/ranvier_test")
            .unwrap();
        let prefix = "tenant; DROP TABLE unrelated;--";
        let persistence = PostgresPersistenceStore::with_table_prefix(pool.clone(), prefix);

        assert_invalid_postgres_prefix(persistence.ensure_schema().await.unwrap_err());
        assert_invalid_postgres_prefix(persistence.append(envelope(1, "Next")).await.unwrap_err());
        assert_invalid_postgres_prefix(persistence.load("trace").await.unwrap_err());
        assert_invalid_postgres_prefix(persistence.resume("trace", 1).await.unwrap_err());
        assert_invalid_postgres_prefix(
            persistence
                .complete("trace", CompletionState::Success)
                .await
                .unwrap_err(),
        );
        assert_invalid_postgres_prefix(
            persistence
                .save_intervention(
                    "trace",
                    Intervention {
                        target_node: "node".into(),
                        payload_override: None,
                        timestamp_ms: 1,
                        expected_event_sequence: 0,
                    },
                )
                .await
                .unwrap_err(),
        );

        let idempotency = PostgresCompensationIdempotencyStore::with_table_prefix(pool, prefix);
        assert_invalid_postgres_prefix(idempotency.ensure_schema().await.unwrap_err());
        assert_invalid_postgres_prefix(idempotency.purge_older_than_ms(1).await.unwrap_err());
        assert_invalid_postgres_prefix(idempotency.was_compensated("key").await.unwrap_err());
        assert_invalid_postgres_prefix(idempotency.mark_compensated("key").await.unwrap_err());
    }

    #[cfg(feature = "persistence-postgres")]
    #[tokio::test]
    async fn postgres_store_rejects_prefix_if_any_table_name_would_be_truncated_before_pool_use() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://user:password@127.0.0.1:1/ranvier_test")
            .unwrap();
        // `_events` fits exactly, but `_interventions` would be truncated. Reject the
        // store as a whole so append/load cannot appear usable with an invalid prefix.
        let prefix = "p".repeat(POSTGRES_DEFAULT_MAX_IDENTIFIER_BYTES - "_events".len());
        let persistence = PostgresPersistenceStore::with_table_prefix(pool, prefix);

        assert_invalid_postgres_prefix(persistence.append(envelope(1, "Next")).await.unwrap_err());
    }

    #[cfg(feature = "persistence-postgres")]
    fn assert_invalid_postgres_prefix(error: anyhow::Error) {
        assert!(
            error
                .to_string()
                .contains("invalid PostgreSQL table prefix"),
            "expected prefix error before DB I/O, got {error:#}"
        );
    }

    fn envelope(step: u64, outcome_kind: &str) -> PersistenceEnvelope {
        PersistenceEnvelope {
            trace_id: "trace-1".to_string(),
            circuit: "OrderCircuit".to_string(),
            schematic_version: "1.0".to_string(),
            event_sequence: step,
            event_type: persistence_event_type_for(outcome_kind, None),
            step,
            node_id: None,
            outcome_kind: outcome_kind.to_string(),
            timestamp_ms: 1_700_000_000_000 + step,
            payload_hash: Some(format!("hash-{}", step)),
            payload: None,
        }
    }

    /// A v2 journal event for `trace_id`.
    #[cfg(feature = "persistence-redis")]
    fn traced_event(
        trace_id: &str,
        sequence: u64,
        node_id: Option<&str>,
        event_type: PersistenceEventType,
        outcome_kind: &str,
    ) -> PersistenceEnvelope {
        let mut event = journal_event(sequence, sequence, node_id, event_type, outcome_kind);
        event.trace_id = trace_id.to_owned();
        event
    }

    fn journal_event(
        sequence: u64,
        step: u64,
        node_id: Option<&str>,
        event_type: PersistenceEventType,
        outcome_kind: &str,
    ) -> PersistenceEnvelope {
        PersistenceEnvelope {
            trace_id: "trace-1".into(),
            circuit: "OrderCircuit".into(),
            schematic_version: "2".into(),
            event_sequence: sequence,
            event_type,
            step,
            node_id: node_id.map(str::to_owned),
            outcome_kind: outcome_kind.into(),
            timestamp_ms: 1_700_000_000_000 + sequence,
            payload_hash: None,
            payload: Some(serde_json::json!({"sequence": sequence})),
        }
    }

    #[tokio::test]
    async fn journal_pairs_intent_and_result_at_the_same_workflow_step() {
        let store = InMemoryPersistenceStore::new();
        store.append(envelope(0, "Enter")).await.unwrap();
        store
            .append(journal_event(
                1,
                0,
                Some("charge-card"),
                PersistenceEventType::Intent,
                "Intent",
            ))
            .await
            .unwrap();
        store
            .append(journal_event(
                2,
                0,
                Some("charge-card"),
                PersistenceEventType::Result,
                "Next",
            ))
            .await
            .unwrap();

        let trace = store.load("trace-1").await.unwrap().unwrap();
        assert_eq!(trace.events[1].step, trace.events[2].step);
        assert_eq!(trace.events[1].event_sequence, 1);
        assert_eq!(trace.events[2].event_sequence, 2);
        assert_eq!(validate_persisted_trace(&trace), Ok(()));
    }

    #[tokio::test]
    async fn unresolved_intent_is_quarantined_and_cannot_be_followed_by_terminal() {
        let store = InMemoryPersistenceStore::new();
        store.append(envelope(0, "Enter")).await.unwrap();
        store
            .append(journal_event(
                1,
                0,
                Some("charge-card"),
                PersistenceEventType::Intent,
                "Intent",
            ))
            .await
            .unwrap();
        let trace = store.load("trace-1").await.unwrap().unwrap();
        assert_eq!(
            validate_persisted_trace(&trace),
            Err(TraceValidationError::PendingIntent)
        );

        assert!(store.resume("trace-1", 0).await.is_err());
        assert!(
            store
                .complete("trace-1", CompletionState::Success)
                .await
                .is_err()
        );
        assert_eq!(store.load("trace-1").await.unwrap(), Some(trace.clone()));

        let mut terminal = envelope(2, "Fault");
        terminal.event_type = PersistenceEventType::Terminal;
        assert!(store.append(terminal).await.is_err());
        assert_eq!(
            store.load("trace-1").await.unwrap().unwrap().events.len(),
            2
        );
    }

    #[tokio::test]
    async fn completion_requires_a_confirmed_terminal_event() {
        let store = InMemoryPersistenceStore::new();
        store.append(envelope(0, "Enter")).await.unwrap();
        let before = store.load("trace-1").await.unwrap().unwrap();

        assert!(
            store
                .complete("trace-1", CompletionState::Success)
                .await
                .is_err()
        );
        assert_eq!(store.load("trace-1").await.unwrap(), Some(before.clone()));

        let mut completed_without_terminal = before;
        completed_without_terminal.completion = Some(CompletionState::Success);
        assert_eq!(
            validate_persisted_trace(&completed_without_terminal),
            Err(TraceValidationError::MissingTerminal)
        );
    }

    #[tokio::test]
    async fn terminal_event_is_final_and_unique() {
        let store = InMemoryPersistenceStore::new();
        store.append(envelope(0, "Enter")).await.unwrap();
        let mut terminal = envelope(1, "Success");
        terminal.event_type = PersistenceEventType::Terminal;
        store.append(terminal).await.unwrap();
        let before = store.load("trace-1").await.unwrap().unwrap();

        let mut event_after_terminal = journal_event(
            2,
            1,
            Some("charge-card"),
            PersistenceEventType::Intent,
            "Intent",
        );
        event_after_terminal.trace_id = "trace-1".into();
        assert!(store.append(event_after_terminal).await.is_err());

        let mut duplicate_terminal = envelope(2, "Success");
        duplicate_terminal.event_type = PersistenceEventType::Terminal;
        assert!(store.append(duplicate_terminal).await.is_err());
        assert_eq!(store.load("trace-1").await.unwrap(), Some(before));
    }

    #[tokio::test]
    async fn rejected_append_preserves_existing_trace_snapshot() {
        let store = InMemoryPersistenceStore::new();
        store.append(envelope(0, "Enter")).await.unwrap();
        let before = store.load("trace-1").await.unwrap();
        let mut wrong_circuit = envelope(1, "Next");
        wrong_circuit.circuit = "AnotherCircuit".into();
        wrong_circuit.schematic_version = "rejected-version".into();
        assert!(store.append(wrong_circuit).await.is_err());
        assert_eq!(store.load("trace-1").await.unwrap(), before);
        let mut accepted = envelope(1, "Next");
        accepted.schematic_version = "accepted-version".into();
        store.append(accepted.clone()).await.unwrap();
        let updated = store.load("trace-1").await.unwrap().unwrap();
        assert_eq!(updated.schematic_version, "accepted-version");
        assert_eq!(updated.events, vec![envelope(0, "Enter"), accepted]);
        store
            .complete("trace-1", CompletionState::Success)
            .await
            .unwrap();
        let completed = store.load("trace-1").await.unwrap();
        let mut late = envelope(2, "Next");
        late.schematic_version = "late-rejected-version".into();
        assert!(store.append(late).await.is_err());
        assert_eq!(store.load("trace-1").await.unwrap(), completed);
    }

    #[tokio::test]
    async fn barrier_concurrent_memory_appends_reject_stale_sequence() {
        let store = InMemoryPersistenceStore::new();
        store.append(envelope(0, "Enter")).await.unwrap();
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mut tasks = Vec::new();
        for step in [1, 1] {
            let store = store.clone();
            let barrier = barrier.clone();
            tasks.push(tokio::spawn(async move {
                let snapshot = store.load("trace-1").await.unwrap().unwrap();
                assert_eq!(snapshot.events.len(), 1);
                tokio::time::timeout(std::time::Duration::from_secs(2), barrier.wait())
                    .await
                    .expect("both writers must reach the barrier");
                store.append(envelope(step, "Next")).await
            }));
        }
        let mut results = Vec::new();
        for mut task in tasks {
            let result = tokio::time::timeout(std::time::Duration::from_secs(4), &mut task).await;
            if result.is_err() {
                task.abort();
                let _ = task.await;
            }
            results.push(result);
        }
        let mut succeeded = 0;
        for result in results {
            if result.expect("bounded writer completion").unwrap().is_ok() {
                succeeded += 1;
            }
        }
        assert_eq!(succeeded, 1);
        let trace = store.load("trace-1").await.unwrap().unwrap();
        assert_eq!(trace.events.len(), 2);
        assert_eq!(trace.events[0], envelope(0, "Enter"));
        assert_eq!(trace.events[1].event_sequence, 1);
    }

    #[tokio::test]
    async fn append_and_load_roundtrip() {
        let store = InMemoryPersistenceStore::new();
        store.append(envelope(0, "Enter")).await.unwrap();
        store.append(envelope(1, "Next")).await.unwrap();

        let loaded = store.load("trace-1").await.unwrap().unwrap();
        assert_eq!(loaded.trace_id, "trace-1");
        assert_eq!(loaded.circuit, "OrderCircuit");
        assert_eq!(loaded.events.len(), 2);
        assert_eq!(loaded.events[0].step, 0);
        assert_eq!(loaded.events[1].outcome_kind, "Next");
        assert_eq!(loaded.completion, None);
    }

    #[tokio::test]
    async fn resume_records_cursor() {
        let store = InMemoryPersistenceStore::new();
        store.append(envelope(0, "Enter")).await.unwrap();
        store.append(envelope(1, "Fault")).await.unwrap();

        let cursor = store.resume("trace-1", 1).await.unwrap();
        assert_eq!(
            cursor,
            ResumeCursor {
                trace_id: "trace-1".to_string(),
                next_step: 2
            }
        );

        let loaded = store.load("trace-1").await.unwrap().unwrap();
        assert_eq!(loaded.resumed_from_step, Some(1));
    }

    #[tokio::test]
    async fn complete_marks_trace_and_blocks_append() {
        let store = InMemoryPersistenceStore::new();
        store.append(envelope(0, "Enter")).await.unwrap();
        store.append(envelope(1, "Next")).await.unwrap();
        store
            .complete("trace-1", CompletionState::Success)
            .await
            .unwrap();

        let loaded = store.load("trace-1").await.unwrap().unwrap();
        assert_eq!(loaded.completion, Some(CompletionState::Success));
        store
            .complete("trace-1", CompletionState::Success)
            .await
            .unwrap();
        assert!(
            store
                .complete("trace-1", CompletionState::Fault)
                .await
                .is_err()
        );
        assert_eq!(store.load("trace-1").await.unwrap(), Some(loaded.clone()));

        for err in [
            store.append(envelope(2, "Next")).await.unwrap_err(),
            store.resume("trace-1", 1).await.unwrap_err(),
        ] {
            assert_eq!(
                err.downcast_ref::<PersistenceConflict>(),
                Some(&PersistenceConflict {
                    trace_id: "trace-1".into(),
                    kind: PersistenceConflictKind::Completed,
                })
            );
            assert!(err.to_string().contains("is already completed"));
        }
    }

    #[tokio::test]
    async fn append_rejects_cross_circuit_trace_reuse() {
        let store = InMemoryPersistenceStore::new();
        store.append(envelope(0, "Enter")).await.unwrap();

        let mut invalid = envelope(1, "Next");
        invalid.circuit = "AnotherCircuit".to_string();
        let err = store.append(invalid).await.unwrap_err();
        assert!(
            err.to_string()
                .contains("already exists for circuit OrderCircuit")
        );
    }

    #[tokio::test]
    async fn in_memory_compensation_idempotency_roundtrip() {
        let store = InMemoryCompensationIdempotencyStore::new();
        let key = "trace-a:OrderFlow:Fault";

        assert!(!store.was_compensated(key).await.unwrap());
        store.mark_compensated(key).await.unwrap();
        assert!(store.was_compensated(key).await.unwrap());
    }

    fn compensation_context(
        trace_id: &str,
        circuit: &str,
        fault_kind: &str,
        fault_step: u64,
        intent_sequence: Option<u64>,
    ) -> CompensationContext {
        CompensationContext {
            trace_id: trace_id.to_string(),
            circuit: circuit.to_string(),
            fault_kind: fault_kind.to_string(),
            fault_step,
            timestamp_ms: 1_700_000_000_000,
            intent_sequence,
        }
    }

    /// Pairs of compensation contexts that name different effects. Each pair
    /// aliased under the version 1 encoding or differs in one framed field.
    fn distinct_compensation_key_pairs(trace: &str) -> Vec<(String, String)> {
        let a_b = format!("{trace}a:b");
        let a = format!("{trace}a");
        let multibyte_colon = format!("{trace}\u{c8fc}\u{bb38}:");
        let multibyte = format!("{trace}\u{c8fc}\u{bb38}");
        let cases = [
            // Same trace, circuit and fault kind; different intent sequence.
            (
                compensation_context(trace, "OrderFlow", "Fault", 3, Some(4)),
                compensation_context(trace, "OrderFlow", "Fault", 3, Some(5)),
            ),
            // Same trace, circuit and fault kind; different step, no trace store.
            (
                compensation_context(trace, "OrderFlow", "Fault", 4, None),
                compensation_context(trace, "OrderFlow", "Fault", 5, None),
            ),
            // Delimiter partitions of trace and circuit.
            (
                compensation_context(&a_b, "c", "d", 1, Some(1)),
                compensation_context(&a, "b:c", "d", 1, Some(1)),
            ),
            (
                compensation_context(&a_b, "c", "d", 1, None),
                compensation_context(&a, "b:c", "d", 1, None),
            ),
            // Fault kind and circuit partition without a trace store.
            (
                compensation_context(trace, "k:x", "Fault", 1, None),
                compensation_context(trace, "x", "Fault:1:k", 1, None),
            ),
            // Journaled and unjournaled keys never meet.
            (
                compensation_context(trace, "OrderFlow", "Fault", 7, Some(7)),
                compensation_context(trace, "OrderFlow", "Fault", 7, None),
            ),
            // Multi-byte text is framed by UTF-8 byte length.
            (
                compensation_context(&multibyte_colon, "flow", "Fault", 1, Some(2)),
                compensation_context(&multibyte, ":flow", "Fault", 1, Some(2)),
            ),
        ];
        cases
            .iter()
            .map(|(first, second)| {
                (
                    compensation_idempotency_key(first),
                    compensation_idempotency_key(second),
                )
            })
            .collect()
    }

    #[test]
    fn compensation_idempotency_key_v2_framing() {
        assert_eq!(
            compensation_idempotency_key(&compensation_context(
                "trace-a",
                "OrderFlow",
                "Fault",
                3,
                Some(7)
            )),
            "v2:j:7:7:trace-a:9:OrderFlow"
        );
        assert_eq!(
            compensation_idempotency_key(&compensation_context(
                "trace-a",
                "OrderFlow",
                "Cancelled",
                3,
                None
            )),
            "v2:u:3:9:Cancelled:7:trace-a:9:OrderFlow"
        );
        // The timestamp and, for journaled keys, the step and kind are not identity.
        let mut later = compensation_context("trace-a", "OrderFlow", "Fault", 3, Some(7));
        later.timestamp_ms += 1;
        later.fault_step = 9;
        later.fault_kind = "Cancelled".to_string();
        assert_eq!(
            compensation_idempotency_key(&later),
            "v2:j:7:7:trace-a:9:OrderFlow"
        );
    }

    #[tokio::test]
    async fn compensation_idempotency_keys_do_not_collide() {
        let store = InMemoryCompensationIdempotencyStore::new();
        for (first, second) in distinct_compensation_key_pairs("") {
            assert_ne!(first, second);
            store.mark_compensated(&first).await.unwrap();
            assert!(store.was_compensated(&first).await.unwrap());
            assert!(
                !store.was_compensated(&second).await.unwrap(),
                "{first} must not mark {second}"
            );
            store.keys.write().await.clear();
        }
    }

    #[cfg(feature = "persistence-postgres")]
    #[tokio::test]
    #[ignore = "requires explicitly isolated backend fixture URL"]
    async fn postgres_store_roundtrip_when_configured() {
        let url = std::env::var("RANVIER_PERSISTENCE_POSTGRES_URL")
            .unwrap_or_else(|_| panic!("explicit isolated backend fixture URL required"));
        assert!(
            !url.trim().is_empty(),
            "explicit backend fixture URL must not be empty"
        );

        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .connect(&url)
            .await
            .unwrap();
        // Every relation name must fit PostgreSQL's 63-byte identifier limit.
        let table_prefix = format!("rpt_{}", &Uuid::new_v4().simple().to_string()[..12]);
        let store = PostgresPersistenceStore::with_table_prefix(pool.clone(), table_prefix.clone());
        store.ensure_schema().await.unwrap();

        let trace_id = format!("trace-{}", Uuid::new_v4().simple());
        let circuit = "PgCircuit".to_string();

        // A v2 journal starts at event sequence 0.
        let mut first = journal_event(0, 0, None, PersistenceEventType::Enter, "Enter");
        first.trace_id = trace_id.clone();
        first.circuit = circuit.clone();
        store.append(first).await.unwrap();

        let mut second = journal_event(1, 1, None, PersistenceEventType::Terminal, "Branch");
        second.trace_id = trace_id.clone();
        second.circuit = circuit.clone();
        store.append(second).await.unwrap();

        let cursor = store.resume(&trace_id, 2).await.unwrap();
        assert_eq!(cursor.next_step, 3);

        store
            .complete(&trace_id, CompletionState::Compensated)
            .await
            .unwrap();
        assert!(store.resume(&trace_id, 2).await.is_err());

        let loaded = store.load(&trace_id).await.unwrap().unwrap();
        assert_eq!(loaded.trace_id, trace_id);
        assert_eq!(loaded.circuit, circuit);
        assert_eq!(loaded.events.len(), 2);
        assert_eq!(loaded.resumed_from_step, Some(2));
        assert_eq!(loaded.completion, Some(CompletionState::Compensated));

        let drop_interventions = format!(
            "DROP TABLE IF EXISTS {}",
            store.interventions_table.sql().unwrap()
        );
        let drop_events = format!("DROP TABLE IF EXISTS {}", store.events_table.sql().unwrap());
        let drop_state = format!("DROP TABLE IF EXISTS {}", store.state_table.sql().unwrap());
        sqlx::query(&drop_interventions)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(&drop_events).execute(&pool).await.unwrap();
        sqlx::query(&drop_state).execute(&pool).await.unwrap();
    }

    #[cfg(feature = "persistence-postgres")]
    #[tokio::test]
    #[ignore = "requires explicitly isolated backend fixture URL"]
    async fn postgres_identifier_prefixes_execute_when_configured() -> anyhow::Result<()> {
        let url = std::env::var("RANVIER_PERSISTENCE_POSTGRES_URL")
            .map_err(|_| anyhow::anyhow!("explicit isolated backend fixture URL required"))?;
        anyhow::ensure!(
            !url.trim().is_empty(),
            "explicit backend fixture URL must not be empty"
        );

        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .connect(&url)
            .await?;
        let suffix = Uuid::new_v4().simple().to_string();
        let prefixes = [
            format!("public.r$1{}", &suffix[..15]),
            format!("public.\"q\"\"{}\"", &suffix[16..]),
        ];
        let guard = format!("ranvier_identifier_guard_{}", Uuid::new_v4().simple());

        let mut cleanup_relations = vec![guard.clone()];
        let fixture_result: anyhow::Result<()> = async {
            for table_prefix in prefixes {
                let store = PostgresPersistenceStore::with_table_prefix(
                    pool.clone(),
                    table_prefix.as_str(),
                );
                let idempotency = PostgresCompensationIdempotencyStore::with_table_prefix(
                    pool.clone(),
                    table_prefix.as_str(),
                );
                cleanup_relations.extend([
                    store.interventions_table.sql()?.to_owned(),
                    store.events_table.sql()?.to_owned(),
                    store.state_table.sql()?.to_owned(),
                    idempotency.table.sql()?.to_owned(),
                ]);

                store.ensure_schema().await?;
                let trace_id = format!("prefix-{}", Uuid::new_v4().simple());
                let mut event = journal_event(0, 0, None, PersistenceEventType::Enter, "Enter");
                event.trace_id = trace_id.clone();
                event.circuit = "IdentifierBoundary".to_string();
                store.append(event).await?;
                store
                    .save_intervention(
                        &trace_id,
                        Intervention {
                            target_node: "node".into(),
                            payload_override: Some(serde_json::json!({"case": "identifier"})),
                            timestamp_ms: 1,
                            expected_event_sequence: 1,
                        },
                    )
                    .await?;

                let cursor = store.resume(&trace_id, 1).await?;
                anyhow::ensure!(cursor.next_step == 2, "unexpected resume cursor");
                let loaded = store
                    .load(&trace_id)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("persisted trace was not found"))?;
                anyhow::ensure!(loaded.trace_id == trace_id, "trace id did not round-trip");
                anyhow::ensure!(loaded.events.len() == 1, "event did not round-trip");
                anyhow::ensure!(
                    loaded.interventions.len() == 1,
                    "intervention did not round-trip"
                );

                let mut terminal =
                    journal_event(1, 1, None, PersistenceEventType::Terminal, "Success");
                terminal.trace_id = trace_id.clone();
                terminal.circuit = "IdentifierBoundary".to_string();
                store.append(terminal).await?;
                store.complete(&trace_id, CompletionState::Success).await?;
                let completed = store
                    .load(&trace_id)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("completed trace was not found"))?;
                anyhow::ensure!(
                    completed.completion == Some(CompletionState::Success),
                    "completion did not round-trip"
                );

                idempotency.ensure_schema().await?;
                let key = format!("key-{}", Uuid::new_v4().simple());
                anyhow::ensure!(
                    !idempotency.was_compensated(&key).await?,
                    "new idempotency key unexpectedly exists"
                );
                idempotency.mark_compensated(&key).await?;
                anyhow::ensure!(
                    idempotency.was_compensated(&key).await?,
                    "idempotency marker did not round-trip"
                );
                let purged = idempotency.purge_older_than_ms(i64::MAX).await?;
                anyhow::ensure!(purged >= 1, "idempotency marker was not purged");
                anyhow::ensure!(
                    !idempotency.was_compensated(&key).await?,
                    "purged idempotency marker still exists"
                );
            }

            sqlx::query(&format!("CREATE TABLE {guard} (id INTEGER PRIMARY KEY)"))
                .execute(&pool)
                .await?;
            let hostile_prefix = format!("{guard}; DROP TABLE {guard};--");
            let hostile_store =
                PostgresPersistenceStore::with_table_prefix(pool.clone(), hostile_prefix);
            let error = hostile_store.ensure_schema().await.err().ok_or_else(|| {
                anyhow::anyhow!("hostile relation prefix unexpectedly reached PostgreSQL")
            })?;
            anyhow::ensure!(
                error
                    .to_string()
                    .contains("invalid PostgreSQL table prefix"),
                "hostile prefix failed for an unexpected reason: {error:#}"
            );

            let guard_exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
                .bind(&guard)
                .fetch_one(&pool)
                .await?;
            anyhow::ensure!(guard_exists, "hostile prefix dropped the guard relation");
            Ok(())
        }
        .await;

        let mut cleanup_error = None;
        for relation in cleanup_relations {
            if let Err(error) = sqlx::query(&format!("DROP TABLE IF EXISTS {relation}"))
                .execute(&pool)
                .await
            {
                if cleanup_error.is_none() {
                    cleanup_error = Some(anyhow::Error::from(error));
                }
            }
        }
        pool.close().await;
        match (fixture_result, cleanup_error) {
            (Ok(()), None) => Ok(()),
            (Ok(()), Some(cleanup_error)) => Err(cleanup_error),
            (Err(fixture_error), None) => Err(fixture_error),
            (Err(fixture_error), Some(cleanup_error)) => Err(anyhow::anyhow!(
                "fixture failed: {fixture_error:#}; cleanup also failed: {cleanup_error:#}"
            )),
        }
    }

    #[cfg(feature = "persistence-redis")]
    #[tokio::test]
    #[ignore = "requires explicitly isolated Redis fixture URL"]
    async fn redis_stale_write_must_not_erase_acknowledged_event() {
        let url =
            std::env::var("RANVIER_ATOMICITY_REDIS_URL").expect("explicit isolated Redis required");
        let store = RedisPersistenceStore::connect(&url).await.unwrap();
        let trace_id = format!("stale-{}", Uuid::new_v4().simple());
        let mut seed = traced_event(&trace_id, 0, None, PersistenceEventType::Enter, "Enter");
        seed.timestamp_ms = u64::MAX;
        seed.payload = Some(serde_json::json!({"large": u64::MAX}));
        store.append(seed.clone()).await.unwrap();
        let (previous, before) = store.read_trace(&trace_id).await.unwrap();
        let before = before.unwrap();
        let (other_previous, other_before) = store.read_trace(&trace_id).await.unwrap();
        assert_eq!(previous, other_previous);
        assert_eq!(Some(before.clone()), other_before);
        let mut first = before.clone();
        let mut second = before.clone();
        // Two writers extend the same snapshot with different events.
        let event_one = traced_event(
            &trace_id,
            1,
            Some("node-one"),
            PersistenceEventType::Checkpoint,
            "Checkpoint",
        );
        let event_two = traced_event(
            &trace_id,
            1,
            Some("node-two"),
            PersistenceEventType::Checkpoint,
            "Checkpoint",
        );
        first.events.push(event_one.clone());
        second.events.push(event_two.clone());
        store
            .write_trace(&first, previous.as_deref())
            .await
            .unwrap();
        let second_result = store.write_trace(&second, other_previous.as_deref()).await;
        let loaded = store.load(&trace_id).await.unwrap().unwrap();
        assert!(
            loaded.events.contains(&event_one),
            "successful first write must remain acknowledged"
        );
        assert!(
            second_result
                .unwrap_err()
                .to_string()
                .contains("concurrent persistence update")
        );
        assert_eq!(
            loaded, first,
            "rejected stale write must preserve entire winning snapshot"
        );
        assert!(
            store.write_trace(&second, None).await.is_err(),
            "absent-key expectation must not overwrite existing trace"
        );
        store
            .append(traced_event(
                &trace_id,
                2,
                None,
                PersistenceEventType::Terminal,
                "Success",
            ))
            .await
            .unwrap();
        let winning_payload = store.read_trace(&trace_id).await.unwrap().0;
        store
            .complete(&trace_id, CompletionState::Success)
            .await
            .unwrap();
        let terminal = store.load(&trace_id).await.unwrap();
        assert!(
            store
                .write_trace(&second, winning_payload.as_deref())
                .await
                .is_err()
        );
        assert_eq!(store.load(&trace_id).await.unwrap(), terminal);
        println!("REDIS_STALE_WRITE_PASS acknowledged-write-preserved stale-write-rejected");
    }

    #[cfg(feature = "persistence-redis")]
    #[tokio::test]
    #[ignore = "requires explicitly isolated Redis fixture URL"]
    async fn redis_mismatched_trace_identity_is_rejected() {
        use redis::AsyncCommands;
        let url =
            std::env::var("RANVIER_ATOMICITY_REDIS_URL").expect("explicit isolated Redis required");
        let store = RedisPersistenceStore::connect(&url).await.unwrap();
        let requested = format!("identity-{}", Uuid::new_v4().simple());
        let mut event = envelope(0, "Next");
        event.trace_id = requested.clone();
        store.append(event.clone()).await.unwrap();
        let mut wrong = store.load(&requested).await.unwrap().unwrap();
        wrong.trace_id = format!("other-{}", Uuid::new_v4().simple());
        let payload = serde_json::to_string(&wrong).unwrap();
        let mut conn = store.manager.clone();
        let _: () = conn.set(store.key(&requested), &payload).await.unwrap();
        assert!(
            store.load(&requested).await.is_err(),
            "stored identity must match requested key"
        );
        assert!(store.append(event).await.is_err());
        let after: String = conn.get(store.key(&requested)).await.unwrap();
        assert_eq!(after, payload);
        let other: Option<String> = conn.get(store.key(&wrong.trace_id)).await.unwrap();
        assert_eq!(
            other, None,
            "corrupt identity must not redirect a write into another trace"
        );
        println!("REDIS_IDENTITY_PASS mismatched-key rejected-read-write unchanged-other-key");
    }

    #[cfg(feature = "persistence-redis")]
    #[tokio::test]
    #[ignore = "requires explicitly isolated Redis fixture URL"]
    async fn redis_public_independent_clients_preserve_acknowledged_appends() {
        use std::sync::Arc;
        use tokio::{
            sync::Barrier,
            task::JoinSet,
            time::{Duration, timeout},
        };
        let url =
            std::env::var("RANVIER_ATOMICITY_REDIS_URL").expect("explicit isolated Redis required");
        let observer = timeout(Duration::from_secs(5), RedisPersistenceStore::connect(&url))
            .await
            .unwrap()
            .unwrap();
        let trace_id = format!("clients-{}", Uuid::new_v4().simple());
        let seed = traced_event(&trace_id, 0, None, PersistenceEventType::Enter, "Enter");
        observer.append(seed.clone()).await.unwrap();
        let mut expected = observer.load(&trace_id).await.unwrap().unwrap();
        let barrier = Arc::new(Barrier::new(17));
        let mut workers = JoinSet::new();
        // Connect before spawning so connection failures cannot strand the barrier.
        for step in 1..=16 {
            let client = timeout(Duration::from_secs(5), RedisPersistenceStore::connect(&url))
                .await
                .unwrap()
                .unwrap();
            let barrier = barrier.clone();
            // Every client appends the next revision with its own event.
            let node = format!("client-{step}");
            let event = traced_event(
                &trace_id,
                1,
                Some(&node),
                PersistenceEventType::Checkpoint,
                "Checkpoint",
            );
            workers.spawn(async move {
                barrier.wait().await;
                let result = client.append(event.clone()).await;
                (event, result)
            });
        }
        let results = timeout(Duration::from_secs(10), async {
            barrier.wait().await;
            let mut accepted = vec![seed];
            let mut conflicts = 0;
            while let Some(result) = workers.join_next().await {
                let (event, result) = result.unwrap();
                match result {
                    Ok(()) => accepted.push(event),
                    Err(error) => {
                        assert!(matches!(
                            error.downcast_ref::<PersistenceConflict>().map(|c| &c.kind),
                            Some(
                                PersistenceConflictKind::ConcurrentUpdate
                                    | PersistenceConflictKind::StaleRevision { .. }
                            )
                        ));
                        conflicts += 1;
                    }
                }
            }
            (accepted, conflicts)
        })
        .await;
        if results.is_err() {
            workers.abort_all();
            while workers.join_next().await.is_some() {}
        }
        let (accepted, conflicts) = results.expect("bounded independent-client writes");
        assert_eq!(accepted.len(), 2, "exactly one public append succeeds");
        assert_eq!(conflicts, 15);
        let loaded = observer.load(&trace_id).await.unwrap().unwrap();
        expected.events = accepted.clone();
        assert_eq!(
            loaded, expected,
            "full snapshot must match acknowledged writes"
        );
        assert_eq!(
            expected.events, accepted,
            "every acknowledged event, and only those events, survive"
        );
        assert_eq!(expected.completion, None);
        let terminal = traced_event(
            &trace_id,
            2,
            None,
            PersistenceEventType::Terminal,
            "Success",
        );
        observer.append(terminal.clone()).await.unwrap();
        expected.events.push(terminal);
        observer
            .complete(&trace_id, CompletionState::Success)
            .await
            .unwrap();
        expected.completion = Some(CompletionState::Success);
        let late_client = RedisPersistenceStore::connect(&url).await.unwrap();
        let late = traced_event(
            &trace_id,
            3,
            Some("late"),
            PersistenceEventType::Checkpoint,
            "Checkpoint",
        );
        assert!(
            late_client
                .append(late)
                .await
                .unwrap_err()
                .to_string()
                .contains("already completed")
        );
        assert_eq!(observer.load(&trace_id).await.unwrap().unwrap(), expected);
        println!(
            "REDIS_CLIENTS_PASS independent-connections acknowledged-events exact-terminal-snapshot"
        );
        println!(
            "Redis clients: accepted={} conflicts={conflicts}",
            accepted.len() - 1
        );
    }

    #[cfg(feature = "persistence-redis")]
    #[tokio::test]
    #[ignore = "requires explicitly isolated Redis fixture URL"]
    async fn redis_public_complete_resume_controlled_orders() {
        // In the v2 journal an append and a completion cannot both be valid on
        // the same snapshot (completion needs the terminal event, which ends
        // appends). Completion therefore races a resume of the terminal trace.
        use std::sync::Arc;
        use tokio::{
            sync::Semaphore,
            task::JoinSet,
            time::{Duration, timeout},
        };
        let url = std::env::var("RANVIER_ATOMICITY_REDIS_URL").expect("explicit Redis fixture");
        let connect = || timeout(Duration::from_secs(5), RedisPersistenceStore::connect(&url));
        for completion_first in [true, false] {
            let observer = connect().await.unwrap().unwrap();
            let trace_id = format!("order-{}", Uuid::new_v4().simple());
            for event in [
                traced_event(&trace_id, 0, None, PersistenceEventType::Enter, "Enter"),
                traced_event(
                    &trace_id,
                    1,
                    None,
                    PersistenceEventType::Terminal,
                    "Success",
                ),
            ] {
                observer.append(event).await.unwrap();
            }
            let mut expected = observer.load(&trace_id).await.unwrap().unwrap();
            let original = observer.read_trace(&trace_id).await.unwrap().0;
            let gate = || {
                Arc::new(RedisWriteGate {
                    ready: Semaphore::new(0),
                    release: Semaphore::new(0),
                    expected: tokio::sync::Mutex::new(None),
                })
            };
            let append_gate = gate();
            let complete_gate = gate();
            let mut appender = connect().await.unwrap().unwrap();
            let mut completer = connect().await.unwrap().unwrap();
            appender.write_gate = Some(append_gate.clone());
            completer.write_gate = Some(complete_gate.clone());
            let resume_id = trace_id.clone();
            let id = trace_id.clone();
            let mut workers = JoinSet::new();
            workers.spawn(async move { (false, appender.resume(&resume_id, 1).await.map(|_| ())) });
            workers.spawn(async move {
                (
                    true,
                    completer.complete(&id, CompletionState::Success).await,
                )
            });
            let result = timeout(Duration::from_secs(10), async {
                append_gate.ready.acquire().await.unwrap().forget();
                complete_gate.ready.acquire().await.unwrap().forget();
                assert_eq!(*append_gate.expected.lock().await, original);
                assert_eq!(*complete_gate.expected.lock().await, original);
                let (first, second) = if completion_first {
                    (&complete_gate, &append_gate)
                } else {
                    (&append_gate, &complete_gate)
                };
                first.release.add_permits(1);
                let (kind, outcome) = workers.join_next().await.unwrap().unwrap();
                assert_eq!(kind, completion_first);
                outcome.unwrap();
                if completion_first {
                    expected.completion = Some(CompletionState::Success);
                } else {
                    expected.resumed_from_step = Some(1);
                }
                assert_eq!(observer.load(&trace_id).await.unwrap().unwrap(), expected);
                second.release.add_permits(1);
                let (kind, outcome) = workers.join_next().await.unwrap().unwrap();
                assert_eq!(kind, !completion_first);
                assert!(
                    outcome
                        .unwrap_err()
                        .to_string()
                        .contains("concurrent persistence update")
                );
                assert_eq!(observer.load(&trace_id).await.unwrap().unwrap(), expected);
                if !completion_first {
                    observer
                        .complete(&trace_id, CompletionState::Success)
                        .await
                        .unwrap();
                    expected.completion = Some(CompletionState::Success);
                }
                let late = traced_event(
                    &trace_id,
                    2,
                    Some("late"),
                    PersistenceEventType::Checkpoint,
                    "Checkpoint",
                );
                assert!(
                    observer
                        .append(late)
                        .await
                        .unwrap_err()
                        .to_string()
                        .contains("already completed")
                );
                assert_eq!(observer.load(&trace_id).await.unwrap().unwrap(), expected);
            })
            .await;
            workers.abort_all();
            while workers.join_next().await.is_some() {}
            result.expect("bounded public-method order fixture");
            println!(
                "REDIS_ORDER_PASS completion_first={completion_first} same-read controlled-writes exact-snapshot"
            );
        }
    }

    #[cfg(feature = "persistence-redis")]
    #[tokio::test]
    #[ignore = "requires explicitly isolated Redis fixture URL"]
    async fn redis_public_resume_intervention_controlled_orders() {
        use tokio::{
            sync::Semaphore,
            task::JoinSet,
            time::{Duration, timeout},
        };
        let url = std::env::var("RANVIER_ATOMICITY_REDIS_URL").expect("explicit Redis fixture");
        let connect = || timeout(Duration::from_secs(5), RedisPersistenceStore::connect(&url));
        for intervention_first in [true, false] {
            let observer = connect().await.unwrap().unwrap();
            let trace_id = format!("resume-order-{}", Uuid::new_v4().simple());
            observer
                .append(traced_event(
                    &trace_id,
                    0,
                    None,
                    PersistenceEventType::Enter,
                    "Enter",
                ))
                .await
                .unwrap();
            let intervention = Intervention {
                target_node: "reviewed-node".to_owned(),
                payload_override: Some(
                    serde_json::json!({"large": u64::MAX, "command": "fixture"}),
                ),
                timestamp_ms: u64::MAX,
                expected_event_sequence: 2,
            };
            // Seed existing fields so a winner must preserve earlier caller data too.
            observer.resume(&trace_id, 2).await.unwrap();
            observer
                .save_intervention(
                    &trace_id,
                    Intervention {
                        target_node: "prior-node".to_owned(),
                        payload_override: Some(serde_json::json!({"prior": true})),
                        timestamp_ms: 1,
                        expected_event_sequence: 1,
                    },
                )
                .await
                .unwrap();
            // Advance the revision so the raced intervention binds to a fresh one.
            observer
                .append(traced_event(
                    &trace_id,
                    1,
                    Some("advance"),
                    PersistenceEventType::Checkpoint,
                    "Checkpoint",
                ))
                .await
                .unwrap();
            let mut expected = observer.load(&trace_id).await.unwrap().unwrap();
            let original = observer.read_trace(&trace_id).await.unwrap().0;
            let gate = || {
                Arc::new(RedisWriteGate {
                    ready: Semaphore::new(0),
                    release: Semaphore::new(0),
                    expected: tokio::sync::Mutex::new(None),
                })
            };
            let resume_gate = gate();
            let intervention_gate = gate();
            let mut resumer = connect().await.unwrap().unwrap();
            let mut editor = connect().await.unwrap().unwrap();
            resumer.write_gate = Some(resume_gate.clone());
            editor.write_gate = Some(intervention_gate.clone());
            let resume_id = trace_id.clone();
            let edit_id = trace_id.clone();
            let edit = intervention.clone();
            let mut workers = JoinSet::new();
            workers.spawn(async move {
                let result = resumer.resume(&resume_id, 7).await.map(|cursor| {
                    assert_eq!(cursor.trace_id, resume_id);
                    assert_eq!(cursor.next_step, 8);
                });
                (false, result)
            });
            workers.spawn(async move { (true, editor.save_intervention(&edit_id, edit).await) });
            let result = timeout(Duration::from_secs(10), async {
                resume_gate.ready.acquire().await.unwrap().forget();
                intervention_gate.ready.acquire().await.unwrap().forget();
                assert_eq!(*resume_gate.expected.lock().await, original);
                assert_eq!(*intervention_gate.expected.lock().await, original);
                let (first, second) = if intervention_first {
                    (&intervention_gate, &resume_gate)
                } else {
                    (&resume_gate, &intervention_gate)
                };
                first.release.add_permits(1);
                let (kind, outcome) = workers.join_next().await.unwrap().unwrap();
                assert_eq!(kind, intervention_first);
                outcome.unwrap();
                if intervention_first {
                    expected.interventions.push(intervention.clone());
                } else {
                    expected.resumed_from_step = Some(7);
                }
                assert_eq!(observer.load(&trace_id).await.unwrap().unwrap(), expected);
                second.release.add_permits(1);
                let (kind, outcome) = workers.join_next().await.unwrap().unwrap();
                assert_eq!(kind, !intervention_first);
                assert!(
                    outcome
                        .unwrap_err()
                        .to_string()
                        .contains("concurrent persistence update")
                );
                assert_eq!(observer.load(&trace_id).await.unwrap().unwrap(), expected);
                // Explicit fresh caller operation, not an automatic adapter retry.
                if intervention_first {
                    let cursor = observer.resume(&trace_id, 7).await.unwrap();
                    assert_eq!(cursor.trace_id, trace_id);
                    assert_eq!(cursor.next_step, 8);
                    expected.resumed_from_step = Some(7);
                } else {
                    observer
                        .save_intervention(&trace_id, intervention.clone())
                        .await
                        .unwrap();
                    expected.interventions.push(intervention.clone());
                }
                assert_eq!(observer.load(&trace_id).await.unwrap().unwrap(), expected);
                let terminal = traced_event(
                    &trace_id,
                    2,
                    None,
                    PersistenceEventType::Terminal,
                    "Success",
                );
                observer.append(terminal.clone()).await.unwrap();
                expected.events.push(terminal);
                observer
                    .complete(&trace_id, CompletionState::Success)
                    .await
                    .unwrap();
                expected.completion = Some(CompletionState::Success);
                assert!(
                    observer
                        .resume(&trace_id, 7)
                        .await
                        .unwrap_err()
                        .to_string()
                        .contains("already completed")
                );
                assert_eq!(observer.load(&trace_id).await.unwrap().unwrap(), expected);
            })
            .await;
            workers.abort_all();
            while workers.join_next().await.is_some() {}
            result.expect("bounded resume/intervention order fixture");
            println!(
                "REDIS_FIELDS_PASS intervention_first={intervention_first} same-read controlled-writes exact-snapshot"
            );
        }
    }

    #[cfg(feature = "persistence-redis")]
    #[tokio::test]
    #[ignore = "requires explicitly isolated backend fixture URL"]
    async fn redis_store_roundtrip_when_configured() {
        let url = std::env::var("RANVIER_PERSISTENCE_REDIS_URL")
            .unwrap_or_else(|_| panic!("explicit isolated backend fixture URL required"));
        assert!(
            !url.trim().is_empty(),
            "explicit backend fixture URL must not be empty"
        );

        let base = RedisPersistenceStore::connect(&url).await.unwrap();
        let prefix = format!("ranvier:persistence:test:{}", Uuid::new_v4().simple());
        let store = RedisPersistenceStore::with_prefix(base.manager.clone(), prefix);

        let trace_id = format!("trace-{}", Uuid::new_v4().simple());
        let circuit = "RedisCircuit".to_string();

        // A v2 journal starts at event sequence 0.
        let mut first = journal_event(0, 0, None, PersistenceEventType::Enter, "Enter");
        first.trace_id = trace_id.clone();
        first.circuit = circuit.clone();
        store.append(first).await.unwrap();

        let mut second = journal_event(1, 1, None, PersistenceEventType::Terminal, "Fault");
        second.trace_id = trace_id.clone();
        second.circuit = circuit.clone();
        store.append(second).await.unwrap();

        let cursor = store.resume(&trace_id, 2).await.unwrap();
        assert_eq!(cursor.next_step, 3);

        store
            .complete(&trace_id, CompletionState::Fault)
            .await
            .unwrap();
        assert!(store.resume(&trace_id, 2).await.is_err());

        let loaded = store.load(&trace_id).await.unwrap().unwrap();
        assert_eq!(loaded.trace_id, trace_id);
        assert_eq!(loaded.circuit, circuit);
        assert_eq!(loaded.events.len(), 2);
        assert_eq!(loaded.resumed_from_step, Some(2));
        assert_eq!(loaded.completion, Some(CompletionState::Fault));

        use redis::AsyncCommands;
        let key = store.key(&trace_id);
        let mut conn = store.manager.clone();
        let _: () = conn.del(key).await.unwrap();
    }

    #[cfg(feature = "persistence-postgres")]
    #[tokio::test]
    #[ignore = "requires explicitly isolated backend fixture URL"]
    async fn postgres_compensation_idempotency_roundtrip_when_configured() {
        let url = std::env::var("RANVIER_PERSISTENCE_POSTGRES_URL")
            .unwrap_or_else(|_| panic!("explicit isolated backend fixture URL required"));
        assert!(
            !url.trim().is_empty(),
            "explicit backend fixture URL must not be empty"
        );

        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .connect(&url)
            .await
            .unwrap();
        let table_prefix = format!("rci_{}", Uuid::new_v4().simple());
        let store =
            PostgresCompensationIdempotencyStore::with_table_prefix(pool.clone(), &table_prefix);
        store.ensure_schema().await.unwrap();

        let key = format!("trace-{}:OrderFlow:Fault", Uuid::new_v4().simple());
        assert!(!store.was_compensated(&key).await.unwrap());
        store.mark_compensated(&key).await.unwrap();
        assert!(store.was_compensated(&key).await.unwrap());
        store.mark_compensated(&key).await.unwrap();
        assert!(store.was_compensated(&key).await.unwrap());

        let drop_table = format!("DROP TABLE IF EXISTS {}", store.table.sql().unwrap());
        sqlx::query(&drop_table).execute(&pool).await.unwrap();
    }

    #[cfg(feature = "persistence-postgres")]
    #[tokio::test]
    #[ignore = "requires explicitly isolated backend fixture URL"]
    async fn postgres_compensation_marker_keys_do_not_collide_when_configured() {
        let url = std::env::var("RANVIER_PERSISTENCE_POSTGRES_URL")
            .unwrap_or_else(|_| panic!("explicit isolated backend fixture URL required"));
        assert!(
            !url.trim().is_empty(),
            "explicit backend fixture URL must not be empty"
        );

        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(3)
            .connect(&url)
            .await
            .unwrap();
        let table_prefix = format!("rci_{}", Uuid::new_v4().simple());
        let store =
            PostgresCompensationIdempotencyStore::with_table_prefix(pool.clone(), &table_prefix);
        store.ensure_schema().await.unwrap();

        let trace = format!("trace-{}-", Uuid::new_v4().simple());
        let pairs = distinct_compensation_key_pairs(&trace);
        let observations: anyhow::Result<()> = async {
            for (first, second) in &pairs {
                anyhow::ensure!(first != second);
                anyhow::ensure!(!store.was_compensated(first).await?);
                anyhow::ensure!(!store.was_compensated(second).await?);
                store.mark_compensated(first).await?;
                anyhow::ensure!(store.was_compensated(first).await?);
                anyhow::ensure!(
                    !store.was_compensated(second).await?,
                    "{first} must not mark {second}"
                );
                store.mark_compensated(second).await?;
                anyhow::ensure!(store.was_compensated(second).await?);
            }
            Ok(())
        }
        .await;

        let drop_table = format!("DROP TABLE IF EXISTS {}", store.table.sql().unwrap());
        let cleanup = sqlx::query(&drop_table).execute(&pool).await;
        observations.unwrap();
        cleanup.unwrap();
    }

    #[cfg(feature = "persistence-postgres")]
    #[tokio::test]
    #[ignore = "requires explicitly isolated backend fixture URL"]
    async fn postgres_compensation_idempotency_purge_when_configured() {
        let url = std::env::var("RANVIER_PERSISTENCE_POSTGRES_URL")
            .unwrap_or_else(|_| panic!("explicit isolated backend fixture URL required"));
        assert!(
            !url.trim().is_empty(),
            "explicit backend fixture URL must not be empty"
        );

        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .connect(&url)
            .await
            .unwrap();
        let table_prefix = format!("rci_{}", Uuid::new_v4().simple());
        let store =
            PostgresCompensationIdempotencyStore::with_table_prefix(pool.clone(), &table_prefix);
        store.ensure_schema().await.unwrap();

        let stale_key = format!("stale-{}", Uuid::new_v4().simple());
        let fresh_key = format!("fresh-{}", Uuid::new_v4().simple());
        store.mark_compensated(&stale_key).await.unwrap();
        store.mark_compensated(&fresh_key).await.unwrap();

        let force_stale_query = format!(
            "UPDATE {}
             SET created_at_ms = 0
             WHERE idempotency_key = $1",
            store.table.sql().unwrap()
        );
        sqlx::query(&force_stale_query)
            .bind(&stale_key)
            .execute(&pool)
            .await
            .unwrap();

        let purged = store.purge_older_than_ms(1).await.unwrap();
        assert!(purged >= 1);
        assert!(!store.was_compensated(&stale_key).await.unwrap());
        assert!(store.was_compensated(&fresh_key).await.unwrap());

        let drop_table = format!("DROP TABLE IF EXISTS {}", store.table.sql().unwrap());
        sqlx::query(&drop_table).execute(&pool).await.unwrap();
    }

    #[cfg(feature = "persistence-redis")]
    #[tokio::test]
    #[ignore = "requires explicitly isolated backend fixture URL"]
    async fn redis_compensation_idempotency_roundtrip_when_configured() {
        let url = std::env::var("RANVIER_PERSISTENCE_REDIS_URL")
            .unwrap_or_else(|_| panic!("explicit isolated backend fixture URL required"));
        assert!(
            !url.trim().is_empty(),
            "explicit backend fixture URL must not be empty"
        );

        let base = RedisCompensationIdempotencyStore::connect(&url)
            .await
            .unwrap();
        let prefix = format!(
            "ranvier:compensation:idempotency:test:{}",
            Uuid::new_v4().simple()
        );
        let store = RedisCompensationIdempotencyStore::with_prefix(base.manager.clone(), prefix);
        let key = format!("trace-{}:OrderFlow:Fault", Uuid::new_v4().simple());

        assert!(!store.was_compensated(&key).await.unwrap());
        store.mark_compensated(&key).await.unwrap();
        assert!(store.was_compensated(&key).await.unwrap());
        store.mark_compensated(&key).await.unwrap();
        assert!(store.was_compensated(&key).await.unwrap());

        use redis::AsyncCommands;
        let mut conn = store.manager.clone();
        let _: () = conn.del(store.key(&key)).await.unwrap();
    }

    #[cfg(feature = "persistence-redis")]
    #[tokio::test]
    #[ignore = "requires explicitly isolated backend fixture URL"]
    async fn redis_compensation_marker_keys_do_not_collide_when_configured() {
        let url = std::env::var("RANVIER_PERSISTENCE_REDIS_URL")
            .unwrap_or_else(|_| panic!("explicit isolated backend fixture URL required"));
        assert!(
            !url.trim().is_empty(),
            "explicit backend fixture URL must not be empty"
        );

        let base = RedisCompensationIdempotencyStore::connect(&url)
            .await
            .unwrap();
        let prefix = format!(
            "ranvier:compensation:idempotency:collision:test:{}",
            Uuid::new_v4().simple()
        );
        let store = RedisCompensationIdempotencyStore::with_prefix(base.manager.clone(), prefix);

        let trace = format!("trace-{}-", Uuid::new_v4().simple());
        let pairs = distinct_compensation_key_pairs(&trace);
        let observations: anyhow::Result<()> = async {
            for (first, second) in &pairs {
                anyhow::ensure!(first != second);
                anyhow::ensure!(!store.was_compensated(first).await?);
                anyhow::ensure!(!store.was_compensated(second).await?);
                store.mark_compensated(first).await?;
                anyhow::ensure!(store.was_compensated(first).await?);
                anyhow::ensure!(
                    !store.was_compensated(second).await?,
                    "{first} must not mark {second}"
                );
                store.mark_compensated(second).await?;
                anyhow::ensure!(store.was_compensated(second).await?);
            }
            Ok(())
        }
        .await;

        use redis::AsyncCommands;
        let mut conn = store.manager.clone();
        let mut cleanup: redis::RedisResult<usize> = Ok(0);
        for (first, second) in &pairs {
            let deleted: redis::RedisResult<usize> =
                conn.del(&[store.key(first), store.key(second)]).await;
            if cleanup.is_ok() {
                cleanup = deleted;
            }
        }
        observations.unwrap();
        cleanup.unwrap();
    }

    #[cfg(feature = "persistence-redis")]
    #[tokio::test]
    #[ignore = "requires explicitly isolated backend fixture URL"]
    async fn redis_compensation_idempotency_ttl_when_configured() {
        let url = std::env::var("RANVIER_PERSISTENCE_REDIS_URL")
            .unwrap_or_else(|_| panic!("explicit isolated backend fixture URL required"));
        assert!(
            !url.trim().is_empty(),
            "explicit backend fixture URL must not be empty"
        );

        let base = RedisCompensationIdempotencyStore::connect(&url)
            .await
            .unwrap();
        let prefix = format!(
            "ranvier:compensation:idempotency:ttl:test:{}",
            Uuid::new_v4().simple()
        );
        let store =
            RedisCompensationIdempotencyStore::with_prefix_and_ttl(base.manager.clone(), prefix, 1);
        let key = format!("ttl-{}", Uuid::new_v4().simple());

        assert!(!store.was_compensated(&key).await.unwrap());
        store.mark_compensated(&key).await.unwrap();
        assert!(store.was_compensated(&key).await.unwrap());
        // The TTL is applied by the same SET command that creates the marker.
        let ttl: i64 = redis::cmd("TTL")
            .arg(store.key(&key))
            .query_async(&mut store.manager.clone())
            .await
            .unwrap();
        assert!((0..=1).contains(&ttl), "ttl = {ttl}");
        // A repeated mark is a no-op and keeps the original expiry.
        store.mark_compensated(&key).await.unwrap();

        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
        assert!(!store.was_compensated(&key).await.unwrap());
    }
}
