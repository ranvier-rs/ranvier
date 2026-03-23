//! # Axon: Executable Decision Tree
//!
//! The `Axon` is the **runtime execution path** of a Typed Decision Tree.
//! It functions as a reusable Typed Decision flow (Axon<In, Out, E>).
//!
//! ## Design Philosophy
//!
//! * **Axon flows, Schematic shows**: Axon executes; Schematic describes
//! * **Builder pattern**: `Axon::start().then().then()`
//! * **Schematic extraction**: Every Axon carries its structural metadata
//!
//! "Axon is the flowing thing, Schematic is the visible thing."

use crate::persistence::{
    CompensationAutoTrigger, CompensationContext, CompensationHandle,
    CompensationIdempotencyHandle, CompensationRetryPolicy, CompletionState,
    PersistenceAutoComplete, PersistenceEnvelope, PersistenceHandle, PersistenceTraceId,
};
use async_trait::async_trait;
use ranvier_audit::{AuditEvent, AuditSink};
use ranvier_core::bus::Bus;
use ranvier_core::cluster::DistributedLock;
use ranvier_core::event::{DlqPolicy, DlqSink};
use ranvier_core::outcome::Outcome;
use ranvier_core::policy::DynamicPolicy;
use ranvier_core::saga::{SagaPolicy, SagaStack};
use ranvier_core::schematic::{
    BusCapabilitySchema, Schematic,
};
use ranvier_core::timeline::{Timeline, TimelineEvent};
use ranvier_core::transition::Transition;

use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::any::type_name;
use std::ffi::OsString;
use std::fs;
use std::future::Future;

use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::Instrument;

/// Configuration for Execution Mode.
#[derive(Clone)]
pub enum ExecutionMode {
    /// Normal, unpartitioned local execution.
    Local,
    /// Singleton execution, ensures only one instance runs across the entire cluster.
    Singleton {
        lock_key: String,
        ttl_ms: u64,
        lock_provider: Arc<dyn DistributedLock>,
    },
}

/// Strategy for parallel step execution.
///
/// Controls how the Axon handles faults during parallel branch execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParallelStrategy {
    /// All parallel steps must succeed; if any faults, return the first fault.
    AllMustSucceed,
    /// Continue even if some steps fail; return first successful result.
    /// If all branches fault, returns the first fault.
    AnyCanFail,
}

/// Type alias for async boxed futures used in Axon execution.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Executor type for Axon steps.
/// Now takes an input state `In`, a resource bundle `Res`, and returns an `Outcome<Out, E>`.
/// Must be Send + Sync to be reusable across threads and clones.
pub type Executor<In, Out, E, Res> =
    Arc<dyn for<'a> Fn(In, &'a Res, &'a mut Bus) -> BoxFuture<'a, Outcome<Out, E>> + Send + Sync>;

/// Manual intervention jump command injected into the Bus.
#[derive(Debug, Clone)]
pub struct ManualJump {
    pub target_node: String,
    pub payload_override: Option<serde_json::Value>,
}

/// Start step index for resumption, injected into the Bus.
#[derive(Debug, Clone, Copy)]
struct StartStep(u64);

/// Persisted state for resumption, injected into the Bus.
#[derive(Debug, Clone)]
struct ResumptionState {
    payload: Option<serde_json::Value>,
}

/// Helper to extract a readable type name from a type.
fn type_name_of<T: ?Sized>() -> String {
    let full = type_name::<T>();
    full.split("::").last().unwrap_or(full).to_string()
}

/// The Axon Builder and Runtime.
///
/// `Axon` represents an executable decision tree.
/// It is reusable and thread-safe.
///
/// ## Example
///
/// ```rust,ignore
/// use ranvier_core::prelude::*;
/// // ...
/// // Start with an identity Axon (In -> In)
/// let axon = Axon::<(), (), _>::new("My Axon")
///     .then(StepA)
///     .then(StepB);
///
/// // Execute multiple times
/// let res1 = axon.execute((), &mut bus1).await;
/// let res2 = axon.execute((), &mut bus2).await;
/// ```
pub struct Axon<In, Out, E, Res = ()> {
    /// The static structure (for visualization/analysis)
    pub schematic: Schematic,
    /// The runtime executor
    pub(crate) executor: Executor<In, Out, E, Res>,
    /// How this Axon is executed across the cluster
    pub execution_mode: ExecutionMode,
    /// Optional persistence store for state inspection
    pub persistence_store: Option<Arc<dyn crate::persistence::PersistenceStore>>,
    /// Optional audit sink for tamper-evident logging of interventions
    pub audit_sink: Option<Arc<dyn AuditSink>>,
    /// Optional dead-letter queue sink for storing failed events
    pub dlq_sink: Option<Arc<dyn DlqSink>>,
    /// Policy for handling event failures
    pub dlq_policy: DlqPolicy,
    /// Optional dynamic (hot-reloadable) DLQ policy — takes precedence over static `dlq_policy`
    pub dynamic_dlq_policy: Option<DynamicPolicy<DlqPolicy>>,
    /// Policy for automated saga compensation
    pub saga_policy: SagaPolicy,
    /// Optional dynamic (hot-reloadable) Saga policy — takes precedence over static `saga_policy`
    pub dynamic_saga_policy: Option<DynamicPolicy<SagaPolicy>>,
    /// Registry for Saga compensation handlers
    pub saga_compensation_registry:
        Arc<std::sync::RwLock<ranvier_core::saga::SagaCompensationRegistry<E, Res>>>,
    /// Optional IAM handle for identity verification at the Schematic boundary
    pub iam_handle: Option<ranvier_core::iam::IamHandle>,
}

/// Schematic export request derived from command-line args/env.
#[derive(Debug, Clone)]
pub struct SchematicExportRequest {
    /// Optional output file path. If omitted, schematic is written to stdout.
    pub output: Option<PathBuf>,
}

impl<In, Out, E, Res> Clone for Axon<In, Out, E, Res> {
    fn clone(&self) -> Self {
        Self {
            schematic: self.schematic.clone(),
            executor: self.executor.clone(),
            execution_mode: self.execution_mode.clone(),
            persistence_store: self.persistence_store.clone(),
            audit_sink: self.audit_sink.clone(),
            dlq_sink: self.dlq_sink.clone(),
            dlq_policy: self.dlq_policy.clone(),
            dynamic_dlq_policy: self.dynamic_dlq_policy.clone(),
            saga_policy: self.saga_policy.clone(),
            dynamic_saga_policy: self.dynamic_saga_policy.clone(),
            saga_compensation_registry: self.saga_compensation_registry.clone(),
            iam_handle: self.iam_handle.clone(),
        }
    }
}

mod builder;
mod executor;
mod parallel;

#[async_trait]
impl<In, Out, E, Res> ranvier_inspector::StateInspector for Axon<In, Out, E, Res>
where
    In: Send + Sync + Serialize + DeserializeOwned + 'static,
    Out: Send + Sync + Serialize + DeserializeOwned + 'static,
    E: Send + Sync + Serialize + DeserializeOwned + std::fmt::Debug + 'static,
    Res: ranvier_core::transition::ResourceRequirement,
{
    async fn get_state(&self, trace_id: &str) -> Option<serde_json::Value> {
        let store = self.persistence_store.as_ref()?;
        let trace = store.load(trace_id).await.ok().flatten()?;
        Some(serde_json::to_value(trace).unwrap_or(serde_json::Value::Null))
    }

    async fn force_resume(
        &self,
        trace_id: &str,
        target_node: &str,
        payload_override: Option<Value>,
    ) -> Result<(), String> {
        let store = self
            .persistence_store
            .as_ref()
            .ok_or("No persistence store attached to Axon")?;

        let intervention = crate::persistence::Intervention {
            target_node: target_node.to_string(),
            payload_override,
            timestamp_ms: now_ms(),
        };

        store
            .save_intervention(trace_id, intervention)
            .await
            .map_err(|e| format!("Failed to save intervention: {}", e))?;

        if let Some(sink) = self.audit_sink.as_ref() {
            let event = AuditEvent::new(
                uuid::Uuid::new_v4().to_string(),
                "Inspector".to_string(),
                "ForceResume".to_string(),
                trace_id.to_string(),
            )
            .with_metadata("target_node", target_node);

            let _ = sink.append(&event).await;
        }

        tracing::info!(trace_id = %trace_id, target_node = %target_node, "Force resume requested via Inspector");
        Ok(())
    }
}

fn schematic_export_request_from_process() -> Option<SchematicExportRequest> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let mut enabled = env_flag_is_true("RANVIER_SCHEMATIC");
    let mut output = std::env::var_os("RANVIER_SCHEMATIC_OUTPUT").map(PathBuf::from);

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].to_string_lossy();

        if arg == "--schematic" {
            enabled = true;
            i += 1;
            continue;
        }

        if arg == "--schematic-output" || arg == "--output" {
            if let Some(next) = args.get(i + 1) {
                output = Some(PathBuf::from(next));
                i += 2;
                continue;
            }
        } else if let Some(value) = arg.strip_prefix("--schematic-output=") {
            output = Some(PathBuf::from(value));
        } else if let Some(value) = arg.strip_prefix("--output=") {
            output = Some(PathBuf::from(value));
        }

        i += 1;
    }

    if enabled {
        Some(SchematicExportRequest { output })
    } else {
        None
    }
}

fn env_flag_is_true(key: &str) -> bool {
    match std::env::var(key) {
        Ok(v) => matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "on" | "yes"),
        Err(_) => false,
    }
}

fn inspector_enabled_from_env() -> bool {
    let raw = std::env::var("RANVIER_INSPECTOR").ok();
    inspector_enabled_from_value(raw.as_deref())
}

fn inspector_enabled_from_value(value: Option<&str>) -> bool {
    match value {
        Some(v) => matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "on" | "yes"),
        None => true,
    }
}

fn inspector_dev_mode_from_env() -> bool {
    let raw = std::env::var("RANVIER_MODE").ok();
    inspector_dev_mode_from_value(raw.as_deref())
}

fn inspector_dev_mode_from_value(value: Option<&str>) -> bool {
    !matches!(
        value.map(|v| v.to_ascii_lowercase()),
        Some(mode) if mode == "prod" || mode == "production"
    )
}

fn maybe_export_timeline<Out, E>(bus: &mut Bus, outcome: &Outcome<Out, E>) {
    let path = match std::env::var("RANVIER_TIMELINE_OUTPUT") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => return,
    };

    let sampled = sampled_by_bus_id(bus.id, timeline_sample_rate());
    let policy = timeline_adaptive_policy();
    let forced = should_force_export(outcome, &policy);
    let should_export = sampled || forced;
    if !should_export {
        record_sampling_stats(false, sampled, forced, "none", &policy);
        return;
    }

    let mut timeline = bus.read::<Timeline>().cloned().unwrap_or_default();
    timeline.sort();

    let mode = std::env::var("RANVIER_TIMELINE_MODE")
        .unwrap_or_else(|_| "overwrite".to_string())
        .to_ascii_lowercase();

    if let Err(err) = write_timeline_with_policy(&path, &mode, timeline) {
        tracing::warn!(
            "Failed to persist timeline file {} (mode={}): {}",
            path,
            mode,
            err
        );
        record_sampling_stats(false, sampled, forced, &mode, &policy);
    } else {
        record_sampling_stats(true, sampled, forced, &mode, &policy);
    }
}

fn extract_panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

fn outcome_type_name<Out, E>(outcome: &Outcome<Out, E>) -> String {
    match outcome {
        Outcome::Next(_) => "Next".to_string(),
        Outcome::Branch(id, _) => format!("Branch:{}", id),
        Outcome::Jump(id, _) => format!("Jump:{}", id),
        Outcome::Emit(event_type, _) => format!("Emit:{}", event_type),
        Outcome::Fault(_) => "Fault".to_string(),
    }
}

fn outcome_kind_name<Out, E>(outcome: &Outcome<Out, E>) -> &'static str {
    match outcome {
        Outcome::Next(_) => "Next",
        Outcome::Branch(_, _) => "Branch",
        Outcome::Jump(_, _) => "Jump",
        Outcome::Emit(_, _) => "Emit",
        Outcome::Fault(_) => "Fault",
    }
}

fn outcome_target<Out, E>(outcome: &Outcome<Out, E>) -> Option<String> {
    match outcome {
        Outcome::Branch(branch_id, _) => Some(branch_id.clone()),
        Outcome::Jump(node_id, _) => Some(node_id.to_string()),
        Outcome::Emit(event_type, _) => Some(event_type.clone()),
        Outcome::Next(_) | Outcome::Fault(_) => None,
    }
}

fn completion_from_outcome<Out, E>(outcome: &Outcome<Out, E>) -> CompletionState {
    match outcome {
        Outcome::Fault(_) => CompletionState::Fault,
        _ => CompletionState::Success,
    }
}

fn persistence_trace_id(bus: &Bus) -> String {
    if let Some(explicit) = bus.read::<PersistenceTraceId>() {
        explicit.0.clone()
    } else {
        format!("{}:{}", bus.id, now_ms())
    }
}

fn persistence_auto_complete(bus: &Bus) -> bool {
    bus.read::<PersistenceAutoComplete>()
        .map(|v| v.0)
        .unwrap_or(true)
}

fn compensation_auto_trigger(bus: &Bus) -> bool {
    bus.read::<CompensationAutoTrigger>()
        .map(|v| v.0)
        .unwrap_or(true)
}

fn compensation_retry_policy(bus: &Bus) -> CompensationRetryPolicy {
    bus.read::<CompensationRetryPolicy>()
        .copied()
        .unwrap_or_default()
}

/// Unwrap the Outcome enum layer from a persisted event payload.
///
/// Events are stored with `outcome.to_json_value()` which serializes the full
/// Outcome enum, e.g. `{"Next": {"order_id": "1001", ...}}`. The resumption
/// handler expects the raw inner value, so we extract it here.
fn unwrap_outcome_payload(payload: Option<&serde_json::Value>) -> Option<serde_json::Value> {
    payload.map(|p| {
        p.get("Next")
            .or_else(|| p.get("Branch"))
            .or_else(|| p.get("Jump"))
            .cloned()
            .unwrap_or_else(|| p.clone())
    })
}

async fn load_persistence_version(
    handle: &PersistenceHandle,
    trace_id: &str,
) -> (
    u64,
    Option<String>,
    Option<crate::persistence::Intervention>,
    Option<String>,
    Option<serde_json::Value>,
) {
    let store = handle.store();
    match store.load(trace_id).await {
        Ok(Some(trace)) => {
            let (next_step, last_node_id, last_payload) =
                if let Some(resume_from_step) = trace.resumed_from_step {
                    let anchor_event = trace
                        .events
                        .iter()
                        .rev()
                        .find(|event| {
                            event.step <= resume_from_step
                                && event.outcome_kind == "Next"
                                && event.payload.is_some()
                        })
                        .or_else(|| {
                            trace.events.iter().rev().find(|event| {
                                event.step <= resume_from_step
                                    && event.outcome_kind != "Fault"
                                    && event.payload.is_some()
                            })
                        })
                        .or_else(|| {
                            trace.events.iter().rev().find(|event| {
                                event.step <= resume_from_step && event.payload.is_some()
                            })
                        })
                        .or_else(|| trace.events.last());

                    (
                        resume_from_step.saturating_add(1),
                        anchor_event.and_then(|event| event.node_id.clone()),
                        anchor_event.and_then(|event| {
                            unwrap_outcome_payload(event.payload.as_ref())
                        }),
                    )
                } else {
                    let last_event = trace.events.last();
                    (
                        last_event
                            .map(|event| event.step.saturating_add(1))
                            .unwrap_or(0),
                        last_event.and_then(|event| event.node_id.clone()),
                        last_event.and_then(|event| {
                            unwrap_outcome_payload(event.payload.as_ref())
                        }),
                    )
                };

            (
                next_step,
                Some(trace.schematic_version),
                trace.interventions.last().cloned(),
                last_node_id,
                last_payload,
            )
        }
        Ok(None) => (0, None, None, None, None),
        Err(err) => {
            tracing::warn!(
                trace_id = %trace_id,
                error = %err,
                "Failed to load persistence trace; falling back to step=0"
            );
            (0, None, None, None, None)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_this_step<In, Out, E, Res>(
    trans: &(impl Transition<In, Out, Resources = Res, Error = E> + Clone + 'static),
    state: In,
    res: &Res,
    bus: &mut Bus,
    node_id: &str,
    node_label: &str,
    bus_policy: &Option<ranvier_core::bus::BusAccessPolicy>,
    step_idx: u64,
) -> Outcome<Out, E>
where
    In: serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static,
    Out: serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static,
    E: serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + Send + Sync + 'static,
    Res: ranvier_core::transition::ResourceRequirement,
{
    let label = trans.label();
    let res_type = std::any::type_name::<Res>()
        .split("::")
        .last()
        .unwrap_or("unknown");

    // Debug pausing
    let should_pause = if let Some(debug) = bus.read::<ranvier_core::debug::DebugControl>() {
        debug.should_pause(node_id)
    } else {
        false
    };

    if should_pause {
        let trace_id = persistence_trace_id(bus);
        tracing::event!(
            target: "ranvier.debugger",
            tracing::Level::INFO,
            trace_id = %trace_id,
            node_id = %node_id,
            "Node paused"
        );

        if let Some(timeline) = bus.read_mut::<Timeline>() {
            timeline.push(TimelineEvent::NodePaused {
                node_id: node_id.to_string(),
                timestamp: now_ms(),
            });
        }
        if let Some(debug) = bus.read::<ranvier_core::debug::DebugControl>() {
            debug.wait().await;
        }
    }

    let enter_ts = now_ms();
    if let Some(timeline) = bus.read_mut::<Timeline>() {
        timeline.push(TimelineEvent::NodeEnter {
            node_id: node_id.to_string(),
            node_label: node_label.to_string(),
            timestamp: enter_ts,
        });
    }

    // Check DLQ retry policy and pre-serialize state for potential retries
    let dlq_retry_config = bus.read::<DlqPolicy>().and_then(|p| {
        if let DlqPolicy::RetryThenDlq {
            max_attempts,
            backoff_ms,
        } = p
        {
            Some((*max_attempts, *backoff_ms))
        } else {
            None
        }
    });
    let retry_state_snapshot = if dlq_retry_config.is_some() {
        serde_json::to_value(&state).ok()
    } else {
        None
    };

    // State capture for Saga - SERIALIZE BEFORE CONSUMPTION
    let saga_snapshot = if let Some(SagaPolicy::Enabled) = bus.read::<SagaPolicy>() {
        Some(serde_json::to_vec(&state).unwrap_or_default())
    } else {
        None
    };

    let node_span = tracing::info_span!(
        "Node",
        ranvier.node = %label,
        ranvier.resource_type = %res_type,
        ranvier.outcome_kind = tracing::field::Empty,
        ranvier.outcome_target = tracing::field::Empty
    );
    let started = std::time::Instant::now();
    bus.set_access_policy(label.clone(), bus_policy.clone());
    let result = trans
        .run(state, res, bus)
        .instrument(node_span.clone())
        .await;
    bus.clear_access_policy();

    // DLQ Retry loop: if first attempt faulted and RetryThenDlq is configured,
    // retry with exponential backoff before giving up.
    let result = if let Outcome::Fault(_) = &result {
        if let (Some((max_attempts, backoff_ms)), Some(snapshot)) =
            (dlq_retry_config, &retry_state_snapshot)
        {
            let mut final_result = result;
            // attempt 1 already done; retry from 2..=max_attempts
            for attempt in 2..=max_attempts {
                let delay = backoff_ms.saturating_mul(2u64.saturating_pow(attempt - 2));

                tracing::info!(
                    ranvier.node = %label,
                    attempt = attempt,
                    max_attempts = max_attempts,
                    backoff_ms = delay,
                    "Retrying faulted node"
                );

                if let Some(timeline) = bus.read_mut::<Timeline>() {
                    timeline.push(TimelineEvent::NodeRetry {
                        node_id: node_id.to_string(),
                        attempt,
                        max_attempts,
                        backoff_ms: delay,
                        timestamp: now_ms(),
                    });
                }

                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;

                if let Ok(retry_state) = serde_json::from_value::<In>(snapshot.clone()) {
                    bus.set_access_policy(label.clone(), bus_policy.clone());
                    let retry_result = trans
                        .run(retry_state, res, bus)
                        .instrument(tracing::info_span!(
                            "NodeRetry",
                            ranvier.node = %label,
                            attempt = attempt
                        ))
                        .await;
                    bus.clear_access_policy();

                    match &retry_result {
                        Outcome::Fault(_) => {
                            final_result = retry_result;
                        }
                        _ => {
                            final_result = retry_result;
                            break;
                        }
                    }
                }
            }
            final_result
        } else {
            result
        }
    } else {
        result
    };

    node_span.record("ranvier.outcome_kind", outcome_kind_name(&result));
    if let Some(target) = outcome_target(&result) {
        node_span.record("ranvier.outcome_target", tracing::field::display(&target));
    }

    // Inject TransitionErrorContext on fault
    if let Outcome::Fault(ref err) = result {
        let pipeline_name = bus
            .read::<ranvier_core::schematic::Schematic>()
            .map(|s| s.name.clone())
            .unwrap_or_default();
        let ctx = ranvier_core::error::TransitionErrorContext {
            pipeline_name: pipeline_name.clone(),
            transition_name: label.clone(),
            step_index: step_idx as usize,
        };
        tracing::error!(
            pipeline = %pipeline_name,
            transition = %label,
            step = step_idx,
            error = ?err,
            "Transition fault"
        );
        bus.insert(ctx);
    }

    let duration_ms = started.elapsed().as_millis() as u64;
    let exit_ts = now_ms();

    if let Some(timeline) = bus.read_mut::<Timeline>() {
        timeline.push(TimelineEvent::NodeExit {
            node_id: node_id.to_string(),
            outcome_type: outcome_type_name(&result),
            duration_ms,
            timestamp: exit_ts,
        });

        if let Outcome::Branch(branch_id, _) = &result {
            timeline.push(TimelineEvent::Branchtaken {
                branch_id: branch_id.clone(),
                timestamp: exit_ts,
            });
        }
    }

    // Push to Saga Stack if Next outcome and snapshot taken
    if let (Outcome::Next(_), Some(snapshot)) = (&result, saga_snapshot)
        && let Some(stack) = bus.read_mut::<SagaStack>()
    {
        stack.push(node_id.to_string(), label.clone(), snapshot);
    }

    if let Some(handle) = bus.read::<PersistenceHandle>() {
        let trace_id = persistence_trace_id(bus);
        let circuit = bus
            .read::<ranvier_core::schematic::Schematic>()
            .map(|s| s.name.clone())
            .unwrap_or_default();
        let version = bus
            .read::<ranvier_core::schematic::Schematic>()
            .map(|s| s.schema_version.clone())
            .unwrap_or_default();

        persist_execution_event(
            handle,
            &trace_id,
            &circuit,
            &version,
            step_idx,
            Some(node_id.to_string()),
            outcome_kind_name(&result),
            Some(result.to_json_value()),
        )
        .await;
    }

    // DLQ reporting — only fires after all retries are exhausted (RetryThenDlq)
    // or immediately (SendToDlq). Drop policy skips entirely.
    if let Outcome::Fault(f) = &result {
        // Read policy and sink, then drop the borrows before mutable timeline access
        let dlq_action = {
            let policy = bus.read::<DlqPolicy>();
            let sink = bus.read::<Arc<dyn DlqSink>>();
            match (sink, policy) {
                (Some(s), Some(p)) if !matches!(p, DlqPolicy::Drop) => Some(s.clone()),
                _ => None,
            }
        };

        if let Some(sink) = dlq_action {
            if let Some((max_attempts, _)) = dlq_retry_config
                && let Some(timeline) = bus.read_mut::<Timeline>()
            {
                timeline.push(TimelineEvent::DlqExhausted {
                    node_id: node_id.to_string(),
                    total_attempts: max_attempts,
                    timestamp: now_ms(),
                });
            }

            let trace_id = persistence_trace_id(bus);
            let circuit = bus
                .read::<ranvier_core::schematic::Schematic>()
                .map(|s| s.name.clone())
                .unwrap_or_default();
            let _ = sink
                .store_dead_letter(
                    &trace_id,
                    &circuit,
                    node_id,
                    &format!("{:?}", f),
                    &serde_json::to_vec(&f).unwrap_or_default(),
                )
                .await;
        }
    }

    result
}

#[allow(clippy::too_many_arguments)]
async fn run_this_compensated_step<Out, Next, E, Res, Comp>(
    trans: &(impl Transition<Out, Next, Resources = Res, Error = E> + Clone + 'static),
    comp: &Comp,
    state: Out,
    res: &Res,
    bus: &mut Bus,
    node_id: &str,
    comp_node_id: &str,
    node_label: &str,
    bus_policy: &Option<ranvier_core::bus::BusAccessPolicy>,
    step_idx: u64,
) -> Outcome<Next, E>
where
    Out: serde::Serialize + serde::de::DeserializeOwned + Clone + Send + Sync + 'static,
    Next: serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static,
    E: serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + Send + Sync + 'static,
    Res: ranvier_core::transition::ResourceRequirement,
    Comp: Transition<Out, (), Resources = Res, Error = E> + Clone + Send + Sync + 'static,
{
    let label = trans.label();

    // Debug pausing
    let should_pause = if let Some(debug) = bus.read::<ranvier_core::debug::DebugControl>() {
        debug.should_pause(node_id)
    } else {
        false
    };

    if should_pause {
        let trace_id = persistence_trace_id(bus);
        tracing::event!(
            target: "ranvier.debugger",
            tracing::Level::INFO,
            trace_id = %trace_id,
            node_id = %node_id,
            "Node paused (compensated)"
        );

        if let Some(timeline) = bus.read_mut::<Timeline>() {
            timeline.push(TimelineEvent::NodePaused {
                node_id: node_id.to_string(),
                timestamp: now_ms(),
            });
        }
        if let Some(debug) = bus.read::<ranvier_core::debug::DebugControl>() {
            debug.wait().await;
        }
    }

    let enter_ts = now_ms();
    if let Some(timeline) = bus.read_mut::<Timeline>() {
        timeline.push(TimelineEvent::NodeEnter {
            node_id: node_id.to_string(),
            node_label: node_label.to_string(),
            timestamp: enter_ts,
        });
    }

    // State capture for Saga - SERIALIZE BEFORE CONSUMPTION
    let saga_snapshot = if let Some(SagaPolicy::Enabled) = bus.read::<SagaPolicy>() {
        Some(serde_json::to_vec(&state).unwrap_or_default())
    } else {
        None
    };

    let node_span = tracing::info_span!("Node", ranvier.node = %label);
    bus.set_access_policy(label.clone(), bus_policy.clone());
    let result = trans
        .run(state.clone(), res, bus)
        .instrument(node_span)
        .await;
    bus.clear_access_policy();

    let duration_ms = 0; // Simplified
    let exit_ts = now_ms();

    if let Some(timeline) = bus.read_mut::<Timeline>() {
        timeline.push(TimelineEvent::NodeExit {
            node_id: node_id.to_string(),
            outcome_type: outcome_type_name(&result),
            duration_ms,
            timestamp: exit_ts,
        });
    }

    // Automated Compensation Trigger
    if let Outcome::Fault(ref err) = result {
        if compensation_auto_trigger(bus) {
            tracing::info!(
                ranvier.node = %label,
                ranvier.compensation.trigger = "saga",
                error = ?err,
                "Saga compensation triggered"
            );

            if let Some(timeline) = bus.read_mut::<Timeline>() {
                timeline.push(TimelineEvent::NodeEnter {
                    node_id: comp_node_id.to_string(),
                    node_label: format!("Compensate: {}", comp.label()),
                    timestamp: exit_ts,
                });
            }

            // Run compensation
            let _ = comp.run(state, res, bus).await;

            if let Some(timeline) = bus.read_mut::<Timeline>() {
                timeline.push(TimelineEvent::NodeExit {
                    node_id: comp_node_id.to_string(),
                    outcome_type: "Compensated".to_string(),
                    duration_ms: 0,
                    timestamp: now_ms(),
                });
            }

            if let Some(handle) = bus.read::<PersistenceHandle>() {
                let trace_id = persistence_trace_id(bus);
                let circuit = bus
                    .read::<ranvier_core::schematic::Schematic>()
                    .map(|s| s.name.clone())
                    .unwrap_or_default();
                let version = bus
                    .read::<ranvier_core::schematic::Schematic>()
                    .map(|s| s.schema_version.clone())
                    .unwrap_or_default();

                persist_execution_event(
                    handle,
                    &trace_id,
                    &circuit,
                    &version,
                    step_idx + 1, // Compensation node index
                    Some(comp_node_id.to_string()),
                    "Compensated",
                    None,
                )
                .await;
            }
        }
    } else if let (Outcome::Next(_), Some(snapshot)) = (&result, saga_snapshot) {
        // Push to Saga Stack if Next outcome and snapshot taken
        if let Some(stack) = bus.read_mut::<SagaStack>() {
            stack.push(node_id.to_string(), label.clone(), snapshot);
        }

        if let Some(handle) = bus.read::<PersistenceHandle>() {
            let trace_id = persistence_trace_id(bus);
            let circuit = bus
                .read::<ranvier_core::schematic::Schematic>()
                .map(|s| s.name.clone())
                .unwrap_or_default();
            let version = bus
                .read::<ranvier_core::schematic::Schematic>()
                .map(|s| s.schema_version.clone())
                .unwrap_or_default();

            persist_execution_event(
                handle,
                &trace_id,
                &circuit,
                &version,
                step_idx,
                Some(node_id.to_string()),
                outcome_kind_name(&result),
                Some(result.to_json_value()),
            )
            .await;
        }
    }

    // DLQ reporting for compensated steps
    if let Outcome::Fault(f) = &result
        && let (Some(sink), Some(policy)) =
            (bus.read::<Arc<dyn DlqSink>>(), bus.read::<DlqPolicy>())
    {
        let should_dlq = !matches!(policy, DlqPolicy::Drop);
        if should_dlq {
            let trace_id = persistence_trace_id(bus);
            let circuit = bus
                .read::<ranvier_core::schematic::Schematic>()
                .map(|s| s.name.clone())
                .unwrap_or_default();
            let _ = sink
                .store_dead_letter(
                    &trace_id,
                    &circuit,
                    node_id,
                    &format!("{:?}", f),
                    &serde_json::to_vec(&f).unwrap_or_default(),
                )
                .await;
        }
    }

    result
}

#[allow(clippy::too_many_arguments)]
pub async fn persist_execution_event(
    handle: &PersistenceHandle,
    trace_id: &str,
    circuit: &str,
    schematic_version: &str,
    step: u64,
    node_id: Option<String>,
    outcome_kind: &str,
    payload: Option<serde_json::Value>,
) {
    let store = handle.store();
    let envelope = PersistenceEnvelope {
        trace_id: trace_id.to_string(),
        circuit: circuit.to_string(),
        schematic_version: schematic_version.to_string(),
        step,
        node_id,
        outcome_kind: outcome_kind.to_string(),
        timestamp_ms: now_ms(),
        payload_hash: None,
        payload,
    };

    if let Err(err) = store.append(envelope).await {
        tracing::warn!(
            trace_id = %trace_id,
            circuit = %circuit,
            step,
            outcome_kind = %outcome_kind,
            error = %err,
            "Failed to append persistence envelope"
        );
    }
}

async fn persist_completion(
    handle: &PersistenceHandle,
    trace_id: &str,
    completion: CompletionState,
) {
    let store = handle.store();
    if let Err(err) = store.complete(trace_id, completion).await {
        tracing::warn!(
            trace_id = %trace_id,
            error = %err,
            "Failed to complete persistence trace"
        );
    }
}

fn compensation_idempotency_key(context: &CompensationContext) -> String {
    format!(
        "{}:{}:{}",
        context.trace_id, context.circuit, context.fault_kind
    )
}

async fn run_compensation(
    handle: &CompensationHandle,
    context: CompensationContext,
    retry_policy: CompensationRetryPolicy,
    idempotency: Option<CompensationIdempotencyHandle>,
) -> bool {
    let hook = handle.hook();
    let key = compensation_idempotency_key(&context);

    if let Some(store_handle) = idempotency.as_ref() {
        let store = store_handle.store();
        match store.was_compensated(&key).await {
            Ok(true) => {
                tracing::info!(
                    trace_id = %context.trace_id,
                    circuit = %context.circuit,
                    key = %key,
                    "Compensation already recorded; skipping duplicate hook execution"
                );
                return true;
            }
            Ok(false) => {}
            Err(err) => {
                tracing::warn!(
                    trace_id = %context.trace_id,
                    key = %key,
                    error = %err,
                    "Failed to check compensation idempotency state"
                );
            }
        }
    }

    let max_attempts = retry_policy.max_attempts.max(1);
    for attempt in 1..=max_attempts {
        match hook.compensate(context.clone()).await {
            Ok(()) => {
                if let Some(store_handle) = idempotency.as_ref() {
                    let store = store_handle.store();
                    if let Err(err) = store.mark_compensated(&key).await {
                        tracing::warn!(
                            trace_id = %context.trace_id,
                            key = %key,
                            error = %err,
                            "Failed to mark compensation idempotency state"
                        );
                    }
                }
                return true;
            }
            Err(err) => {
                let is_last = attempt == max_attempts;
                tracing::warn!(
                    trace_id = %context.trace_id,
                    circuit = %context.circuit,
                    fault_kind = %context.fault_kind,
                    fault_step = context.fault_step,
                    attempt,
                    max_attempts,
                    error = %err,
                    "Compensation hook attempt failed"
                );
                if !is_last && retry_policy.backoff_ms > 0 {
                    tokio::time::sleep(tokio::time::Duration::from_millis(retry_policy.backoff_ms))
                        .await;
                }
            }
        }
    }
    false
}

fn ensure_timeline(bus: &mut Bus) -> bool {
    if bus.has::<Timeline>() {
        false
    } else {
        bus.insert(Timeline::new());
        true
    }
}

fn should_attach_timeline(bus: &Bus) -> bool {
    // Respect explicitly provided timeline collector from caller.
    if bus.has::<Timeline>() {
        return true;
    }

    // Attach timeline when runtime export path exists.
    has_timeline_output_path()
}

fn has_timeline_output_path() -> bool {
    std::env::var("RANVIER_TIMELINE_OUTPUT")
        .ok()
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
}

fn timeline_sample_rate() -> f64 {
    std::env::var("RANVIER_TIMELINE_SAMPLE_RATE")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .map(|v| v.clamp(0.0, 1.0))
        .unwrap_or(1.0)
}

fn sampled_by_bus_id(bus_id: uuid::Uuid, rate: f64) -> bool {
    if rate <= 0.0 {
        return false;
    }
    if rate >= 1.0 {
        return true;
    }
    let bucket = (bus_id.as_u128() % 10_000) as f64 / 10_000.0;
    bucket < rate
}

fn timeline_adaptive_policy() -> String {
    std::env::var("RANVIER_TIMELINE_ADAPTIVE")
        .unwrap_or_else(|_| "fault_branch".to_string())
        .to_ascii_lowercase()
}

fn should_force_export<Out, E>(outcome: &Outcome<Out, E>, policy: &str) -> bool {
    match policy {
        "off" => false,
        "fault_only" => matches!(outcome, Outcome::Fault(_)),
        "fault_branch_emit" => {
            matches!(
                outcome,
                Outcome::Fault(_) | Outcome::Branch(_, _) | Outcome::Emit(_, _)
            )
        }
        _ => matches!(outcome, Outcome::Fault(_) | Outcome::Branch(_, _)),
    }
}

#[derive(Default, Clone)]
struct SamplingStats {
    total_decisions: u64,
    exported: u64,
    skipped: u64,
    sampled_exports: u64,
    forced_exports: u64,
    last_mode: String,
    last_policy: String,
    last_updated_ms: u64,
}

static TIMELINE_SAMPLING_STATS: OnceLock<Mutex<SamplingStats>> = OnceLock::new();

fn stats_cell() -> &'static Mutex<SamplingStats> {
    TIMELINE_SAMPLING_STATS.get_or_init(|| Mutex::new(SamplingStats::default()))
}

fn record_sampling_stats(exported: bool, sampled: bool, forced: bool, mode: &str, policy: &str) {
    let snapshot = {
        let mut stats = match stats_cell().lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };

        stats.total_decisions += 1;
        if exported {
            stats.exported += 1;
        } else {
            stats.skipped += 1;
        }
        if sampled && exported {
            stats.sampled_exports += 1;
        }
        if forced && exported {
            stats.forced_exports += 1;
        }
        stats.last_mode = mode.to_string();
        stats.last_policy = policy.to_string();
        stats.last_updated_ms = now_ms();
        stats.clone()
    };

    tracing::debug!(
        ranvier.timeline.total_decisions = snapshot.total_decisions,
        ranvier.timeline.exported = snapshot.exported,
        ranvier.timeline.skipped = snapshot.skipped,
        ranvier.timeline.sampled_exports = snapshot.sampled_exports,
        ranvier.timeline.forced_exports = snapshot.forced_exports,
        ranvier.timeline.mode = %snapshot.last_mode,
        ranvier.timeline.policy = %snapshot.last_policy,
        "Timeline sampling stats updated"
    );

    if let Some(path) = timeline_stats_output_path() {
        let payload = serde_json::json!({
            "total_decisions": snapshot.total_decisions,
            "exported": snapshot.exported,
            "skipped": snapshot.skipped,
            "sampled_exports": snapshot.sampled_exports,
            "forced_exports": snapshot.forced_exports,
            "last_mode": snapshot.last_mode,
            "last_policy": snapshot.last_policy,
            "last_updated_ms": snapshot.last_updated_ms
        });
        if let Some(parent) = Path::new(&path).parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Err(err) = fs::write(&path, payload.to_string()) {
            tracing::warn!("Failed to write timeline sampling stats {}: {}", path, err);
        }
    }
}

fn timeline_stats_output_path() -> Option<String> {
    std::env::var("RANVIER_TIMELINE_STATS_OUTPUT")
        .ok()
        .filter(|v| !v.trim().is_empty())
}

fn write_timeline_with_policy(
    path: &str,
    mode: &str,
    mut timeline: Timeline,
) -> Result<(), String> {
    match mode {
        "append" => {
            if Path::new(path).exists() {
                let content = fs::read_to_string(path).map_err(|e| e.to_string())?;
                match serde_json::from_str::<Timeline>(&content) {
                    Ok(mut existing) => {
                        existing.events.append(&mut timeline.events);
                        existing.sort();
                        if let Some(max_events) = max_events_limit() {
                            truncate_timeline_events(&mut existing, max_events);
                        }
                        write_timeline_json(path, &existing)
                    }
                    Err(_) => {
                        // Fallback: if existing is invalid, replace with current timeline
                        if let Some(max_events) = max_events_limit() {
                            truncate_timeline_events(&mut timeline, max_events);
                        }
                        write_timeline_json(path, &timeline)
                    }
                }
            } else {
                if let Some(max_events) = max_events_limit() {
                    truncate_timeline_events(&mut timeline, max_events);
                }
                write_timeline_json(path, &timeline)
            }
        }
        "rotate" => {
            let rotated_path = rotated_path(path, now_ms());
            write_timeline_json(rotated_path.to_string_lossy().as_ref(), &timeline)?;
            if let Some(keep) = rotate_keep_limit() {
                cleanup_rotated_files(path, keep)?;
            }
            Ok(())
        }
        _ => write_timeline_json(path, &timeline),
    }
}

fn write_timeline_json(path: &str, timeline: &Timeline) -> Result<(), String> {
    if let Some(parent) = Path::new(path).parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(timeline).map_err(|e| e.to_string())?;
    fs::write(path, json).map_err(|e| e.to_string())
}

fn rotated_path(path: &str, suffix: u64) -> PathBuf {
    let p = Path::new(path);
    let parent = p.parent().unwrap_or_else(|| Path::new(""));
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("timeline");
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("json");
    parent.join(format!("{}_{}.{}", stem, suffix, ext))
}

fn max_events_limit() -> Option<usize> {
    std::env::var("RANVIER_TIMELINE_MAX_EVENTS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
}

fn rotate_keep_limit() -> Option<usize> {
    std::env::var("RANVIER_TIMELINE_ROTATE_KEEP")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
}

fn truncate_timeline_events(timeline: &mut Timeline, max_events: usize) {
    let len = timeline.events.len();
    if len > max_events {
        let keep_from = len - max_events;
        timeline.events = timeline.events.split_off(keep_from);
    }
}

fn cleanup_rotated_files(base_path: &str, keep: usize) -> Result<(), String> {
    let p = Path::new(base_path);
    let parent = p.parent().unwrap_or_else(|| Path::new("."));
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("timeline");
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("json");
    let prefix = format!("{}_", stem);
    let suffix = format!(".{}", ext);

    let mut files = fs::read_dir(parent)
        .map_err(|e| e.to_string())?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with(&prefix) && name.ends_with(&suffix)
        })
        .map(|entry| {
            let modified = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            (entry.path(), modified)
        })
        .collect::<Vec<_>>();

    files.sort_by(|a, b| b.1.cmp(&a.1));
    for (path, _) in files.into_iter().skip(keep) {
        let _ = fs::remove_file(path);
    }
    Ok(())
}

fn bus_capability_schema_from_policy(
    policy: Option<ranvier_core::bus::BusAccessPolicy>,
) -> Option<BusCapabilitySchema> {
    let policy = policy?;

    let mut allow = policy
        .allow
        .unwrap_or_default()
        .into_iter()
        .map(|entry| entry.type_name.to_string())
        .collect::<Vec<_>>();
    let mut deny = policy
        .deny
        .into_iter()
        .map(|entry| entry.type_name.to_string())
        .collect::<Vec<_>>();
    allow.sort();
    deny.sort();

    if allow.is_empty() && deny.is_empty() {
        return None;
    }

    Some(BusCapabilitySchema { allow, deny })
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{
        Axon, inspector_dev_mode_from_value, inspector_enabled_from_value, sampled_by_bus_id,
        should_force_export,
    };
    use crate::persistence::{
        CompensationContext, CompensationHandle, CompensationHook, CompensationIdempotencyHandle,
        CompensationIdempotencyStore, CompensationRetryPolicy, CompletionState,
        InMemoryCompensationIdempotencyStore, InMemoryPersistenceStore, PersistenceAutoComplete,
        PersistenceHandle, PersistenceStore, PersistenceTraceId,
    };
    use anyhow::Result;
    use async_trait::async_trait;
    use ranvier_audit::{AuditError, AuditEvent, AuditSink};
    use ranvier_core::event::{DlqPolicy, DlqSink};
    use ranvier_core::saga::SagaStack;
    use ranvier_core::timeline::{Timeline, TimelineEvent};
    use ranvier_core::{Bus, BusAccessPolicy, BusTypeRef, Outcome, Transition};
    use serde::{Deserialize, Serialize};
    use std::sync::Arc;
    use tokio::sync::Mutex;
    use uuid::Uuid;

    struct MockAuditSink {
        events: Arc<Mutex<Vec<AuditEvent>>>,
    }

    #[async_trait]
    impl AuditSink for MockAuditSink {
        async fn append(&self, event: &AuditEvent) -> Result<(), AuditError> {
            self.events.lock().await.push(event.clone());
            Ok(())
        }
    }

    #[tokio::test]
    async fn execute_logs_audit_events_for_intervention() {
        use ranvier_inspector::StateInspector;

        let trace_id = "test-audit-trace";
        let store_impl = InMemoryPersistenceStore::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = MockAuditSink {
            events: events.clone(),
        };

        let axon = Axon::<i32, i32, TestInfallible>::start("AuditTest")
            .then(AddOne)
            .with_persistence_store(store_impl.clone())
            .with_audit_sink(sink);

        let mut bus = Bus::new();
        bus.insert(PersistenceHandle::from_arc(Arc::new(store_impl.clone())));
        bus.insert(PersistenceTraceId::new(trace_id));
        let target_node_id = axon.schematic.nodes[0].id.clone();

        // 0. Pre-requisite: Save an initial trace state so intervention has a target to attach to
        store_impl
            .append(crate::persistence::PersistenceEnvelope {
                trace_id: trace_id.to_string(),
                circuit: "AuditTest".to_string(),
                schematic_version: "v1.0".to_string(),
                step: 0,
                node_id: None,
                outcome_kind: "Next".to_string(),
                timestamp_ms: 0,
                payload_hash: None,
                payload: None,
            })
            .await
            .unwrap();

        // 1. Trigger force_resume (should log ForceResume)
        axon.force_resume(trace_id, &target_node_id, None)
            .await
            .unwrap();

        // 2. Execute (should log ApplyIntervention)
        axon.execute(10, &(), &mut bus).await;

        let recorded = events.lock().await;
        assert_eq!(
            recorded.len(),
            2,
            "Should have 2 audit events: ForceResume and ApplyIntervention"
        );
        assert_eq!(recorded[0].action, "ForceResume");
        assert_eq!(recorded[0].target, trace_id);
        assert_eq!(recorded[1].action, "ApplyIntervention");
        assert_eq!(recorded[1].target, trace_id);
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    pub enum TestInfallible {}

    #[test]
    fn inspector_enabled_flag_matrix() {
        assert!(inspector_enabled_from_value(None));
        assert!(inspector_enabled_from_value(Some("1")));
        assert!(inspector_enabled_from_value(Some("true")));
        assert!(inspector_enabled_from_value(Some("on")));
        assert!(!inspector_enabled_from_value(Some("0")));
        assert!(!inspector_enabled_from_value(Some("false")));
    }

    #[test]
    fn inspector_dev_mode_matrix() {
        assert!(inspector_dev_mode_from_value(None));
        assert!(inspector_dev_mode_from_value(Some("dev")));
        assert!(inspector_dev_mode_from_value(Some("staging")));
        assert!(!inspector_dev_mode_from_value(Some("prod")));
        assert!(!inspector_dev_mode_from_value(Some("production")));
    }

    #[test]
    fn adaptive_policy_force_export_matrix() {
        let next = Outcome::<(), &'static str>::Next(());
        let branch = Outcome::<(), &'static str>::Branch("declined".to_string(), None);
        let emit = Outcome::<(), &'static str>::Emit("audit".to_string(), None);
        let fault = Outcome::<(), &'static str>::Fault("boom");

        assert!(!should_force_export(&next, "off"));
        assert!(!should_force_export(&fault, "off"));

        assert!(!should_force_export(&branch, "fault_only"));
        assert!(should_force_export(&fault, "fault_only"));

        assert!(should_force_export(&branch, "fault_branch"));
        assert!(!should_force_export(&emit, "fault_branch"));
        assert!(should_force_export(&fault, "fault_branch"));

        assert!(should_force_export(&branch, "fault_branch_emit"));
        assert!(should_force_export(&emit, "fault_branch_emit"));
        assert!(should_force_export(&fault, "fault_branch_emit"));
    }

    #[test]
    fn sampling_and_adaptive_combination_decisions() {
        let bus_id = Uuid::nil();
        let next = Outcome::<(), &'static str>::Next(());
        let fault = Outcome::<(), &'static str>::Fault("boom");

        let sampled_never = sampled_by_bus_id(bus_id, 0.0);
        assert!(!sampled_never);
        assert!(!(sampled_never || should_force_export(&next, "off")));
        assert!(sampled_never || should_force_export(&fault, "fault_only"));

        let sampled_always = sampled_by_bus_id(bus_id, 1.0);
        assert!(sampled_always);
        assert!(sampled_always || should_force_export(&next, "off"));
        assert!(sampled_always || should_force_export(&fault, "off"));
    }

    #[derive(Clone)]
    struct AddOne;

    #[async_trait]
    impl Transition<i32, i32> for AddOne {
        type Error = TestInfallible;
        type Resources = ();

        async fn run(
            &self,
            state: i32,
            _resources: &Self::Resources,
            _bus: &mut Bus,
        ) -> Outcome<i32, Self::Error> {
            Outcome::Next(state + 1)
        }
    }

    #[derive(Clone)]
    struct AlwaysFault;

    #[async_trait]
    impl Transition<i32, i32> for AlwaysFault {
        type Error = String;
        type Resources = ();

        async fn run(
            &self,
            _state: i32,
            _resources: &Self::Resources,
            _bus: &mut Bus,
        ) -> Outcome<i32, Self::Error> {
            Outcome::Fault("boom".to_string())
        }
    }

    #[derive(Clone)]
    struct CapabilityGuarded;

    #[async_trait]
    impl Transition<(), ()> for CapabilityGuarded {
        type Error = String;
        type Resources = ();

        fn bus_access_policy(&self) -> Option<BusAccessPolicy> {
            Some(BusAccessPolicy::allow_only(vec![BusTypeRef::of::<i32>()]))
        }

        async fn run(
            &self,
            _state: (),
            _resources: &Self::Resources,
            bus: &mut Bus,
        ) -> Outcome<(), Self::Error> {
            match bus.get::<String>() {
                Ok(_) => Outcome::Next(()),
                Err(err) => Outcome::Fault(err.to_string()),
            }
        }
    }

    #[derive(Clone)]
    struct RecordingCompensationHook {
        calls: Arc<Mutex<Vec<CompensationContext>>>,
        should_fail: bool,
    }

    #[async_trait]
    impl CompensationHook for RecordingCompensationHook {
        async fn compensate(&self, context: CompensationContext) -> Result<()> {
            self.calls.lock().await.push(context);
            if self.should_fail {
                return Err(anyhow::anyhow!("compensation failed"));
            }
            Ok(())
        }
    }

    #[derive(Clone)]
    struct FlakyCompensationHook {
        calls: Arc<Mutex<u32>>,
        failures_remaining: Arc<Mutex<u32>>,
    }

    #[async_trait]
    impl CompensationHook for FlakyCompensationHook {
        async fn compensate(&self, _context: CompensationContext) -> Result<()> {
            {
                let mut calls = self.calls.lock().await;
                *calls += 1;
            }
            let mut failures_remaining = self.failures_remaining.lock().await;
            if *failures_remaining > 0 {
                *failures_remaining -= 1;
                return Err(anyhow::anyhow!("transient compensation failure"));
            }
            Ok(())
        }
    }

    #[derive(Clone)]
    struct FailingCompensationIdempotencyStore {
        read_calls: Arc<Mutex<u32>>,
        write_calls: Arc<Mutex<u32>>,
    }

    #[async_trait]
    impl CompensationIdempotencyStore for FailingCompensationIdempotencyStore {
        async fn was_compensated(&self, _key: &str) -> Result<bool> {
            let mut read_calls = self.read_calls.lock().await;
            *read_calls += 1;
            Err(anyhow::anyhow!("forced idempotency read failure"))
        }

        async fn mark_compensated(&self, _key: &str) -> Result<()> {
            let mut write_calls = self.write_calls.lock().await;
            *write_calls += 1;
            Err(anyhow::anyhow!("forced idempotency write failure"))
        }
    }

    #[tokio::test]
    async fn execute_persists_success_trace_when_handle_exists() {
        let store_impl = Arc::new(InMemoryPersistenceStore::new());
        let store_dyn: Arc<dyn PersistenceStore> = store_impl.clone();

        let mut bus = Bus::new();
        bus.insert(PersistenceHandle::from_arc(store_dyn));
        bus.insert(PersistenceTraceId::new("trace-success"));

        let axon = Axon::<i32, i32, TestInfallible>::start("PersistSuccess").then(AddOne);
        let outcome = axon.execute(41, &(), &mut bus).await;
        assert!(matches!(outcome, Outcome::Next(42)));

        let persisted = store_impl.load("trace-success").await.unwrap().unwrap();
        assert_eq!(persisted.events.len(), 3); // Enter + step-level Next + final Next
        assert_eq!(persisted.events[0].outcome_kind, "Enter");
        assert_eq!(persisted.events[1].outcome_kind, "Next"); // step-level
        assert_eq!(persisted.events[2].outcome_kind, "Next"); // final
        assert_eq!(persisted.completion, Some(CompletionState::Success));
    }

    #[tokio::test]
    async fn execute_persists_fault_completion_state() {
        let store_impl = Arc::new(InMemoryPersistenceStore::new());
        let store_dyn: Arc<dyn PersistenceStore> = store_impl.clone();

        let mut bus = Bus::new();
        bus.insert(PersistenceHandle::from_arc(store_dyn));
        bus.insert(PersistenceTraceId::new("trace-fault"));

        let axon = Axon::<i32, i32, String>::start("PersistFault").then(AlwaysFault);
        let outcome = axon.execute(41, &(), &mut bus).await;
        assert!(matches!(outcome, Outcome::Fault(msg) if msg == "boom"));

        let persisted = store_impl.load("trace-fault").await.unwrap().unwrap();
        assert_eq!(persisted.events.len(), 3); // Enter + step-level Fault + final Fault
        assert_eq!(persisted.events[1].outcome_kind, "Fault"); // step-level
        assert_eq!(persisted.events[2].outcome_kind, "Fault"); // final
        assert_eq!(persisted.completion, Some(CompletionState::Fault));
    }

    #[tokio::test]
    async fn execute_respects_persistence_auto_complete_off() {
        let store_impl = Arc::new(InMemoryPersistenceStore::new());
        let store_dyn: Arc<dyn PersistenceStore> = store_impl.clone();

        let mut bus = Bus::new();
        bus.insert(PersistenceHandle::from_arc(store_dyn));
        bus.insert(PersistenceTraceId::new("trace-no-complete"));
        bus.insert(PersistenceAutoComplete(false));

        let axon = Axon::<i32, i32, TestInfallible>::start("PersistNoComplete").then(AddOne);
        let outcome = axon.execute(1, &(), &mut bus).await;
        assert!(matches!(outcome, Outcome::Next(2)));

        let persisted = store_impl.load("trace-no-complete").await.unwrap().unwrap();
        assert_eq!(persisted.events.len(), 3); // Enter + step-level Next + final Next
        assert_eq!(persisted.completion, None);
    }

    #[tokio::test]
    async fn fault_triggers_compensation_and_marks_compensated() {
        let store_impl = Arc::new(InMemoryPersistenceStore::new());
        let store_dyn: Arc<dyn PersistenceStore> = store_impl.clone();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let compensation = RecordingCompensationHook {
            calls: calls.clone(),
            should_fail: false,
        };

        let mut bus = Bus::new();
        bus.insert(PersistenceHandle::from_arc(store_dyn));
        bus.insert(PersistenceTraceId::new("trace-compensated"));
        bus.insert(CompensationHandle::from_hook(compensation));

        let axon = Axon::<i32, i32, String>::start("CompensatedFault").then(AlwaysFault);
        let outcome = axon.execute(7, &(), &mut bus).await;
        assert!(matches!(outcome, Outcome::Fault(msg) if msg == "boom"));

        let persisted = store_impl.load("trace-compensated").await.unwrap().unwrap();
        assert_eq!(persisted.events.len(), 4); // Enter + step-level Fault + final Fault + Compensated
        assert_eq!(persisted.events[0].outcome_kind, "Enter");
        assert_eq!(persisted.events[1].outcome_kind, "Fault"); // step-level
        assert_eq!(persisted.events[2].outcome_kind, "Fault"); // final
        assert_eq!(persisted.events[3].outcome_kind, "Compensated");
        assert_eq!(persisted.completion, Some(CompletionState::Compensated));

        let recorded = calls.lock().await;
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].trace_id, "trace-compensated");
        assert_eq!(recorded[0].fault_kind, "Fault");
    }

    #[tokio::test]
    async fn failed_compensation_keeps_fault_completion() {
        let store_impl = Arc::new(InMemoryPersistenceStore::new());
        let store_dyn: Arc<dyn PersistenceStore> = store_impl.clone();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let compensation = RecordingCompensationHook {
            calls: calls.clone(),
            should_fail: true,
        };

        let mut bus = Bus::new();
        bus.insert(PersistenceHandle::from_arc(store_dyn));
        bus.insert(PersistenceTraceId::new("trace-compensation-failed"));
        bus.insert(CompensationHandle::from_hook(compensation));

        let axon = Axon::<i32, i32, String>::start("CompensationFails").then(AlwaysFault);
        let outcome = axon.execute(7, &(), &mut bus).await;
        assert!(matches!(outcome, Outcome::Fault(msg) if msg == "boom"));

        let persisted = store_impl
            .load("trace-compensation-failed")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted.events.len(), 3); // Enter + step-level Fault + final Fault
        assert_eq!(persisted.events[2].outcome_kind, "Fault"); // final
        assert_eq!(persisted.completion, Some(CompletionState::Fault));

        let recorded = calls.lock().await;
        assert_eq!(recorded.len(), 1);
    }

    #[tokio::test]
    async fn compensation_retry_policy_succeeds_after_retries() {
        let store_impl = Arc::new(InMemoryPersistenceStore::new());
        let store_dyn: Arc<dyn PersistenceStore> = store_impl.clone();
        let calls = Arc::new(Mutex::new(0u32));
        let failures_remaining = Arc::new(Mutex::new(2u32));
        let compensation = FlakyCompensationHook {
            calls: calls.clone(),
            failures_remaining,
        };

        let mut bus = Bus::new();
        bus.insert(PersistenceHandle::from_arc(store_dyn));
        bus.insert(PersistenceTraceId::new("trace-retry-success"));
        bus.insert(CompensationHandle::from_hook(compensation));
        bus.insert(CompensationRetryPolicy {
            max_attempts: 3,
            backoff_ms: 0,
        });

        let axon = Axon::<i32, i32, String>::start("CompensationRetry").then(AlwaysFault);
        let outcome = axon.execute(7, &(), &mut bus).await;
        assert!(matches!(outcome, Outcome::Fault(msg) if msg == "boom"));

        let persisted = store_impl
            .load("trace-retry-success")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted.completion, Some(CompletionState::Compensated));
        assert_eq!(
            persisted.events.last().map(|e| e.outcome_kind.as_str()),
            Some("Compensated")
        );

        let attempt_count = calls.lock().await;
        assert_eq!(*attempt_count, 3);
    }

    #[tokio::test]
    async fn compensation_idempotency_skips_duplicate_hook_execution() {
        let store_impl = Arc::new(InMemoryPersistenceStore::new());
        let store_dyn: Arc<dyn PersistenceStore> = store_impl.clone();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let compensation = RecordingCompensationHook {
            calls: calls.clone(),
            should_fail: false,
        };
        let idempotency = InMemoryCompensationIdempotencyStore::new();

        let mut bus = Bus::new();
        bus.insert(PersistenceHandle::from_arc(store_dyn));
        bus.insert(PersistenceTraceId::new("trace-idempotent"));
        bus.insert(PersistenceAutoComplete(false));
        bus.insert(CompensationHandle::from_hook(compensation));
        bus.insert(CompensationIdempotencyHandle::from_store(idempotency));

        let axon = Axon::<i32, i32, String>::start("CompensationIdempotency").then(AlwaysFault);

        let outcome1 = axon.execute(7, &(), &mut bus).await;
        let outcome2 = axon.execute(8, &(), &mut bus).await;
        assert!(matches!(outcome1, Outcome::Fault(msg) if msg == "boom"));
        assert!(matches!(outcome2, Outcome::Fault(msg) if msg == "boom"));

        let persisted = store_impl.load("trace-idempotent").await.unwrap().unwrap();
        assert_eq!(persisted.completion, None);
        // Verify that "Compensated" events are present for both executions
        let compensated_count = persisted
            .events
            .iter()
            .filter(|e| e.outcome_kind == "Compensated")
            .count();
        assert_eq!(
            compensated_count, 2,
            "Should have 2 Compensated events (one per execution)"
        );

        let recorded = calls.lock().await;
        assert_eq!(recorded.len(), 1);
    }

    #[tokio::test]
    async fn compensation_idempotency_store_failure_does_not_block_compensation() {
        let store_impl = Arc::new(InMemoryPersistenceStore::new());
        let store_dyn: Arc<dyn PersistenceStore> = store_impl.clone();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let read_calls = Arc::new(Mutex::new(0u32));
        let write_calls = Arc::new(Mutex::new(0u32));
        let compensation = RecordingCompensationHook {
            calls: calls.clone(),
            should_fail: false,
        };
        let idempotency = FailingCompensationIdempotencyStore {
            read_calls: read_calls.clone(),
            write_calls: write_calls.clone(),
        };

        let mut bus = Bus::new();
        bus.insert(PersistenceHandle::from_arc(store_dyn));
        bus.insert(PersistenceTraceId::new("trace-idempotency-store-failure"));
        bus.insert(CompensationHandle::from_hook(compensation));
        bus.insert(CompensationIdempotencyHandle::from_store(idempotency));

        let axon = Axon::<i32, i32, String>::start("IdempotencyStoreFailure").then(AlwaysFault);
        let outcome = axon.execute(9, &(), &mut bus).await;
        assert!(matches!(outcome, Outcome::Fault(msg) if msg == "boom"));

        let persisted = store_impl
            .load("trace-idempotency-store-failure")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted.completion, Some(CompletionState::Compensated));
        assert_eq!(
            persisted.events.last().map(|e| e.outcome_kind.as_str()),
            Some("Compensated")
        );

        let recorded = calls.lock().await;
        assert_eq!(recorded.len(), 1);
        assert_eq!(*read_calls.lock().await, 1);
        assert_eq!(*write_calls.lock().await, 1);
    }

    #[tokio::test]
    async fn transition_bus_policy_blocks_unauthorized_resource_access() {
        let mut bus = Bus::new();
        bus.insert(1_i32);
        bus.insert("secret".to_string());

        let axon = Axon::<(), (), String>::start("BusPolicy").then(CapabilityGuarded);
        let outcome = axon.execute((), &(), &mut bus).await;

        match outcome {
            Outcome::Fault(msg) => {
                assert!(msg.contains("Bus access denied"), "{msg}");
                assert!(msg.contains("CapabilityGuarded"), "{msg}");
                assert!(msg.contains("alloc::string::String"), "{msg}");
            }
            other => panic!("expected fault, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_fails_on_version_mismatch_without_migration() {
        let store_impl = Arc::new(InMemoryPersistenceStore::new());
        let store_dyn: Arc<dyn PersistenceStore> = store_impl.clone();

        let trace_id = "v-mismatch";
        // Create an existing trace with an older version
        let old_envelope = crate::persistence::PersistenceEnvelope {
            trace_id: trace_id.to_string(),
            circuit: "TestCircuit".to_string(),
            schematic_version: "0.9".to_string(),
            step: 0,
            node_id: None,
            outcome_kind: "Enter".to_string(),
            timestamp_ms: 0,
            payload_hash: None,
            payload: None,
        };
        store_impl.append(old_envelope).await.unwrap();

        let mut bus = Bus::new();
        bus.insert(PersistenceHandle::from_arc(store_dyn));
        bus.insert(PersistenceTraceId::new(trace_id));

        // Current axon is version 1.0
        let axon = Axon::<i32, i32, TestInfallible>::new("TestCircuit").then(AddOne);
        let outcome = axon.execute(10, &(), &mut bus).await;

        if let Outcome::Emit(kind, _) = outcome {
            assert_eq!(kind, "execution.resumption.version_mismatch_failed");
        } else {
            panic!("Expected version mismatch emission, got {:?}", outcome);
        }
    }

    #[tokio::test]
    async fn execute_resumes_from_start_on_migration_strategy() {
        let store_impl = Arc::new(InMemoryPersistenceStore::new());
        let store_dyn: Arc<dyn PersistenceStore> = store_impl.clone();

        let trace_id = "v-migration";
        // Create an existing trace with an older version at step 5
        let old_envelope = crate::persistence::PersistenceEnvelope {
            trace_id: trace_id.to_string(),
            circuit: "TestCircuit".to_string(),
            schematic_version: "0.9".to_string(),
            step: 5,
            node_id: None,
            outcome_kind: "Next".to_string(),
            timestamp_ms: 0,
            payload_hash: None,
            payload: None,
        };
        store_impl.append(old_envelope).await.unwrap();

        let mut registry = ranvier_core::schematic::MigrationRegistry::new("TestCircuit");
        registry.register(ranvier_core::schematic::SnapshotMigration {
            name: Some("v0.9 to v1.0".to_string()),
            from_version: "0.9".to_string(),
            to_version: "1.0".to_string(),
            default_strategy: ranvier_core::schematic::MigrationStrategy::ResumeFromStart,
            node_mapping: std::collections::HashMap::new(),
            payload_mapper: None,
        });

        let mut bus = Bus::new();
        bus.insert(PersistenceHandle::from_arc(store_dyn));
        bus.insert(PersistenceTraceId::new(trace_id));
        bus.insert(registry);

        let axon = Axon::<i32, i32, TestInfallible>::new("TestCircuit").then(AddOne);
        let outcome = axon.execute(10, &(), &mut bus).await;

        // Should have resumed from start (step 0), resulting in 11
        assert!(matches!(outcome, Outcome::Next(11)));

        // Verify new event has current version
        let persisted = store_impl.load(trace_id).await.unwrap().unwrap();
        assert_eq!(persisted.schematic_version, "1.0");
    }

    #[tokio::test]
    async fn execute_applies_manual_intervention_jump_and_payload() {
        let store_impl = Arc::new(InMemoryPersistenceStore::new());
        let store_dyn: Arc<dyn PersistenceStore> = store_impl.clone();

        let trace_id = "intervention-test";
        // 1. Run a normal trace part-way
        let axon = Axon::<i32, i32, TestInfallible>::new("TestCircuit")
            .then(AddOne)
            .then(AddOne);

        let mut bus = Bus::new();
        bus.insert(PersistenceHandle::from_arc(store_dyn));
        bus.insert(PersistenceTraceId::new(trace_id));

        // Save an intervention: Jump to the second 'AddOne' node (which has the label 'AddOne')
        // with a payload override of 100.
        // The first node is 'AddOne', the second is ALSO 'AddOne'.
        // Schematic position: 0=Ingress, 1=AddOne, 2=AddOne
        let _target_node_label = "AddOne";
        // To be precise, let's find the ID of the second node.
        let target_node_id = axon.schematic.nodes[2].id.clone();

        // Pre-seed an initial trace entry so save_intervention doesn't fail
        store_impl
            .append(crate::persistence::PersistenceEnvelope {
                trace_id: trace_id.to_string(),
                circuit: "TestCircuit".to_string(),
                schematic_version: "1.0".to_string(),
                step: 0,
                node_id: None,
                outcome_kind: "Enter".to_string(),
                timestamp_ms: 0,
                payload_hash: None,
                payload: None,
            })
            .await
            .unwrap();

        store_impl
            .save_intervention(
                trace_id,
                crate::persistence::Intervention {
                    target_node: target_node_id.clone(),
                    payload_override: Some(serde_json::json!(100)),
                    timestamp_ms: 0,
                },
            )
            .await
            .unwrap();

        // 2. Execute. It should skip the first AddOne and use 100 for the second AddOne.
        // Result should be 100 + 1 = 101.
        let outcome = axon.execute(10, &(), &mut bus).await;

        match outcome {
            Outcome::Next(val) => assert_eq!(val, 101, "Should have used payload 100 and added 1"),
            other => panic!("Expected Outcome::Next(101), got {:?}", other),
        }

        // Verify the jump was logged in trace
        let persisted = store_impl.load(trace_id).await.unwrap().unwrap();
        // The last event should be from the jump target's execution.
        assert_eq!(persisted.interventions.len(), 1);
        assert_eq!(persisted.interventions[0].target_node, target_node_id);
    }

    // ── DLQ Retry Tests ──────────────────────────────────────────────

    /// A transition that fails a configurable number of times before succeeding.
    #[derive(Clone)]
    struct FailNThenSucceed {
        remaining: Arc<tokio::sync::Mutex<u32>>,
    }

    #[async_trait]
    impl Transition<i32, i32> for FailNThenSucceed {
        type Error = String;
        type Resources = ();

        async fn run(
            &self,
            state: i32,
            _resources: &Self::Resources,
            _bus: &mut Bus,
        ) -> Outcome<i32, Self::Error> {
            let mut rem = self.remaining.lock().await;
            if *rem > 0 {
                *rem -= 1;
                Outcome::Fault("transient failure".to_string())
            } else {
                Outcome::Next(state + 1)
            }
        }
    }

    /// A mock DLQ sink that records all dead letters.
    #[derive(Clone)]
    struct MockDlqSink {
        letters: Arc<tokio::sync::Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl DlqSink for MockDlqSink {
        async fn store_dead_letter(
            &self,
            workflow_id: &str,
            _circuit_label: &str,
            node_id: &str,
            error_msg: &str,
            _payload: &[u8],
        ) -> Result<(), String> {
            let entry = format!("{}:{}:{}", workflow_id, node_id, error_msg);
            self.letters.lock().await.push(entry);
            Ok(())
        }
    }

    #[tokio::test]
    async fn retry_then_dlq_retries_and_succeeds_before_exhaustion() {
        // Fail 2 times, succeed on 3rd attempt. Policy allows 5 attempts.
        let remaining = Arc::new(tokio::sync::Mutex::new(2u32));
        let trans = FailNThenSucceed { remaining };

        let dlq_sink = MockDlqSink {
            letters: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        };

        let mut bus = Bus::new();
        bus.insert(Timeline::new());

        let axon = Axon::<i32, i32, String>::start("RetrySucceed")
            .then(trans)
            .with_dlq_policy(DlqPolicy::RetryThenDlq {
                max_attempts: 5,
                backoff_ms: 1,
            })
            .with_dlq_sink(dlq_sink.clone());
        let outcome = axon.execute(10, &(), &mut bus).await;

        // Should succeed (10 + 1 = 11)
        assert!(
            matches!(outcome, Outcome::Next(11)),
            "Expected Next(11), got {:?}",
            outcome
        );

        // No dead letters since it recovered
        let letters = dlq_sink.letters.lock().await;
        assert!(
            letters.is_empty(),
            "Should have 0 dead letters, got {}",
            letters.len()
        );

        // Timeline should contain NodeRetry events
        let timeline = bus.read::<Timeline>().unwrap();
        let retry_count = timeline
            .events
            .iter()
            .filter(|e| matches!(e, TimelineEvent::NodeRetry { .. }))
            .count();
        assert_eq!(retry_count, 2, "Should have 2 retry events");
    }

    #[tokio::test]
    async fn retry_then_dlq_exhausts_retries_and_sends_to_dlq() {
        // Always fails. Policy allows 3 attempts (1 initial + 2 retries).
        let mut bus = Bus::new();
        bus.insert(Timeline::new());

        let dlq_sink = MockDlqSink {
            letters: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        };

        let axon = Axon::<i32, i32, String>::start("RetryExhaust")
            .then(AlwaysFault)
            .with_dlq_policy(DlqPolicy::RetryThenDlq {
                max_attempts: 3,
                backoff_ms: 1,
            })
            .with_dlq_sink(dlq_sink.clone());
        let outcome = axon.execute(42, &(), &mut bus).await;

        assert!(
            matches!(outcome, Outcome::Fault(ref msg) if msg == "boom"),
            "Expected Fault(boom), got {:?}",
            outcome
        );

        // Should have exactly 1 dead letter
        let letters = dlq_sink.letters.lock().await;
        assert_eq!(letters.len(), 1, "Should have 1 dead letter");

        // Timeline should have 2 retry events and 1 DlqExhausted event
        let timeline = bus.read::<Timeline>().unwrap();
        let retry_count = timeline
            .events
            .iter()
            .filter(|e| matches!(e, TimelineEvent::NodeRetry { .. }))
            .count();
        let dlq_count = timeline
            .events
            .iter()
            .filter(|e| matches!(e, TimelineEvent::DlqExhausted { .. }))
            .count();
        assert_eq!(
            retry_count, 2,
            "Should have 2 retry events (attempts 2 and 3)"
        );
        assert_eq!(dlq_count, 1, "Should have 1 DlqExhausted event");
    }

    #[tokio::test]
    async fn send_to_dlq_policy_sends_immediately_without_retry() {
        let mut bus = Bus::new();
        bus.insert(Timeline::new());

        let dlq_sink = MockDlqSink {
            letters: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        };

        let axon = Axon::<i32, i32, String>::start("SendDlq")
            .then(AlwaysFault)
            .with_dlq_policy(DlqPolicy::SendToDlq)
            .with_dlq_sink(dlq_sink.clone());
        let outcome = axon.execute(1, &(), &mut bus).await;

        assert!(matches!(outcome, Outcome::Fault(_)));

        // Should have exactly 1 dead letter (immediate, no retries)
        let letters = dlq_sink.letters.lock().await;
        assert_eq!(letters.len(), 1);

        // No retry or DlqExhausted events
        let timeline = bus.read::<Timeline>().unwrap();
        let retry_count = timeline
            .events
            .iter()
            .filter(|e| matches!(e, TimelineEvent::NodeRetry { .. }))
            .count();
        assert_eq!(retry_count, 0);
    }

    #[tokio::test]
    async fn drop_policy_does_not_send_to_dlq() {
        let mut bus = Bus::new();

        let dlq_sink = MockDlqSink {
            letters: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        };

        let axon = Axon::<i32, i32, String>::start("DropDlq")
            .then(AlwaysFault)
            .with_dlq_policy(DlqPolicy::Drop)
            .with_dlq_sink(dlq_sink.clone());
        let outcome = axon.execute(1, &(), &mut bus).await;

        assert!(matches!(outcome, Outcome::Fault(_)));

        // No dead letters
        let letters = dlq_sink.letters.lock().await;
        assert!(letters.is_empty());
    }

    #[tokio::test]
    async fn dynamic_policy_hot_reload_changes_dlq_behavior() {
        use ranvier_core::policy::DynamicPolicy;

        // Start with Drop policy (no DLQ)
        let (tx, dynamic) = DynamicPolicy::new(DlqPolicy::Drop);
        let dlq_sink = MockDlqSink {
            letters: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        };

        let axon = Axon::<i32, i32, String>::start("DynamicDlq")
            .then(AlwaysFault)
            .with_dynamic_dlq_policy(dynamic)
            .with_dlq_sink(dlq_sink.clone());

        // First execution: Drop policy → no dead letters
        let mut bus = Bus::new();
        let outcome = axon.execute(1, &(), &mut bus).await;
        assert!(matches!(outcome, Outcome::Fault(_)));
        assert!(
            dlq_sink.letters.lock().await.is_empty(),
            "Drop policy should produce no DLQ entries"
        );

        // Hot-reload: switch to SendToDlq
        tx.send(DlqPolicy::SendToDlq).unwrap();

        // Second execution: SendToDlq policy → dead letter captured
        let mut bus2 = Bus::new();
        let outcome2 = axon.execute(2, &(), &mut bus2).await;
        assert!(matches!(outcome2, Outcome::Fault(_)));
        assert_eq!(
            dlq_sink.letters.lock().await.len(),
            1,
            "SendToDlq policy should produce 1 DLQ entry"
        );
    }

    #[tokio::test]
    async fn dynamic_saga_policy_hot_reload() {
        use ranvier_core::policy::DynamicPolicy;
        use ranvier_core::saga::SagaPolicy;

        // Start with Disabled saga
        let (tx, dynamic) = DynamicPolicy::new(SagaPolicy::Disabled);

        let axon = Axon::<i32, i32, TestInfallible>::start("DynamicSaga")
            .then(AddOne)
            .with_dynamic_saga_policy(dynamic);

        // First execution: Disabled → no SagaStack in bus
        let mut bus = Bus::new();
        let _outcome = axon.execute(1, &(), &mut bus).await;
        assert!(
            bus.read::<SagaStack>().is_none() || bus.read::<SagaStack>().unwrap().is_empty(),
            "SagaStack should be absent or empty when disabled"
        );

        // Hot-reload: enable saga
        tx.send(SagaPolicy::Enabled).unwrap();

        // Second execution: Enabled → SagaStack populated
        let mut bus2 = Bus::new();
        let _outcome2 = axon.execute(10, &(), &mut bus2).await;
        assert!(
            bus2.read::<SagaStack>().is_some(),
            "SagaStack should exist when saga is enabled"
        );
    }

    // ── IAM Boundary Tests ──────────────────────────────────────

    mod iam_tests {
        use super::*;
        use ranvier_core::iam::{IamError, IamIdentity, IamPolicy, IamToken, IamVerifier};

        /// Mock IamVerifier that returns a fixed identity.
        #[derive(Clone)]
        struct MockVerifier {
            identity: IamIdentity,
            should_fail: bool,
        }

        #[async_trait]
        impl IamVerifier for MockVerifier {
            async fn verify(&self, _token: &str) -> Result<IamIdentity, IamError> {
                if self.should_fail {
                    Err(IamError::InvalidToken("mock verification failure".into()))
                } else {
                    Ok(self.identity.clone())
                }
            }
        }

        #[tokio::test]
        async fn iam_require_identity_passes_with_valid_token() {
            let verifier = MockVerifier {
                identity: IamIdentity::new("alice").with_role("user"),
                should_fail: false,
            };

            let axon = Axon::<i32, i32, TestInfallible>::new("IamTest")
                .with_iam(IamPolicy::RequireIdentity, verifier)
                .then(AddOne);

            let mut bus = Bus::new();
            bus.insert(IamToken("valid-token".to_string()));
            let outcome = axon.execute(10, &(), &mut bus).await;

            assert!(matches!(outcome, Outcome::Next(11)));
            // Verify IamIdentity was injected into Bus
            let identity = bus
                .read::<IamIdentity>()
                .expect("IamIdentity should be in Bus");
            assert_eq!(identity.subject, "alice");
        }

        #[tokio::test]
        async fn iam_require_identity_rejects_missing_token() {
            let verifier = MockVerifier {
                identity: IamIdentity::new("ignored"),
                should_fail: false,
            };

            let axon = Axon::<i32, i32, TestInfallible>::new("IamNoToken")
                .with_iam(IamPolicy::RequireIdentity, verifier)
                .then(AddOne);

            let mut bus = Bus::new();
            // No IamToken inserted
            let outcome = axon.execute(10, &(), &mut bus).await;

            // Should emit missing_token event
            match &outcome {
                Outcome::Emit(label, _) => {
                    assert_eq!(label, "iam.missing_token");
                }
                other => panic!("Expected Emit(iam.missing_token), got {:?}", other),
            }
        }

        #[tokio::test]
        async fn iam_rejects_failed_verification() {
            let verifier = MockVerifier {
                identity: IamIdentity::new("ignored"),
                should_fail: true,
            };

            let axon = Axon::<i32, i32, TestInfallible>::new("IamBadToken")
                .with_iam(IamPolicy::RequireIdentity, verifier)
                .then(AddOne);

            let mut bus = Bus::new();
            bus.insert(IamToken("bad-token".to_string()));
            let outcome = axon.execute(10, &(), &mut bus).await;

            match &outcome {
                Outcome::Emit(label, _) => {
                    assert_eq!(label, "iam.verification_failed");
                }
                other => panic!("Expected Emit(iam.verification_failed), got {:?}", other),
            }
        }

        #[tokio::test]
        async fn iam_require_role_passes_with_matching_role() {
            let verifier = MockVerifier {
                identity: IamIdentity::new("bob").with_role("admin").with_role("user"),
                should_fail: false,
            };

            let axon = Axon::<i32, i32, TestInfallible>::new("IamRole")
                .with_iam(IamPolicy::RequireRole("admin".into()), verifier)
                .then(AddOne);

            let mut bus = Bus::new();
            bus.insert(IamToken("token".to_string()));
            let outcome = axon.execute(5, &(), &mut bus).await;

            assert!(matches!(outcome, Outcome::Next(6)));
        }

        #[tokio::test]
        async fn iam_require_role_denies_without_role() {
            let verifier = MockVerifier {
                identity: IamIdentity::new("carol").with_role("user"),
                should_fail: false,
            };

            let axon = Axon::<i32, i32, TestInfallible>::new("IamRoleDeny")
                .with_iam(IamPolicy::RequireRole("admin".into()), verifier)
                .then(AddOne);

            let mut bus = Bus::new();
            bus.insert(IamToken("token".to_string()));
            let outcome = axon.execute(5, &(), &mut bus).await;

            match &outcome {
                Outcome::Emit(label, _) => {
                    assert_eq!(label, "iam.policy_denied");
                }
                other => panic!("Expected Emit(iam.policy_denied), got {:?}", other),
            }
        }

        #[tokio::test]
        async fn iam_policy_none_skips_verification() {
            let verifier = MockVerifier {
                identity: IamIdentity::new("ignored"),
                should_fail: true, // would fail if actually called
            };

            let axon = Axon::<i32, i32, TestInfallible>::new("IamNone")
                .with_iam(IamPolicy::None, verifier)
                .then(AddOne);

            let mut bus = Bus::new();
            // No token needed when policy is None
            let outcome = axon.execute(10, &(), &mut bus).await;

            assert!(matches!(outcome, Outcome::Next(11)));
        }
    }

    // ── Schema Propagation Tests (M201-RQ9, RQ12) ──────────────────

    #[derive(Clone)]
    struct SchemaTransition;

    #[async_trait]
    impl Transition<String, String> for SchemaTransition {
        type Error = String;
        type Resources = ();

        fn input_schema(&self) -> Option<serde_json::Value> {
            Some(serde_json::json!({
                "type": "object",
                "required": ["name"],
                "properties": {
                    "name": { "type": "string" }
                }
            }))
        }

        async fn run(
            &self,
            state: String,
            _resources: &Self::Resources,
            _bus: &mut Bus,
        ) -> Outcome<String, Self::Error> {
            Outcome::Next(state)
        }
    }

    #[test]
    fn then_auto_populates_input_schema_from_transition() {
        let axon = Axon::<String, String, String>::new("SchemaTest").then(SchemaTransition);

        // The last node (added by .then()) should have input_schema
        let last_node = axon.schematic.nodes.last().unwrap();
        assert!(last_node.input_schema.is_some());
        let schema = last_node.input_schema.as_ref().unwrap();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"][0], "name");
    }

    #[test]
    fn then_leaves_input_schema_none_when_not_provided() {
        let axon = Axon::<i32, i32, TestInfallible>::new("NoSchema").then(AddOne);

        let last_node = axon.schematic.nodes.last().unwrap();
        assert!(last_node.input_schema.is_none());
    }

    #[test]
    fn with_input_schema_value_sets_on_last_node() {
        let schema = serde_json::json!({"type": "integer"});
        let axon = Axon::<i32, i32, TestInfallible>::new("ManualSchema")
            .then(AddOne)
            .with_input_schema_value(schema.clone());

        let last_node = axon.schematic.nodes.last().unwrap();
        assert_eq!(last_node.input_schema.as_ref().unwrap(), &schema);
    }

    #[test]
    fn with_output_schema_value_sets_on_last_node() {
        let schema = serde_json::json!({"type": "integer"});
        let axon = Axon::<i32, i32, TestInfallible>::new("OutputSchema")
            .then(AddOne)
            .with_output_schema_value(schema.clone());

        let last_node = axon.schematic.nodes.last().unwrap();
        assert_eq!(last_node.output_schema.as_ref().unwrap(), &schema);
    }

    #[test]
    fn schematic_export_includes_schema_fields() {
        let axon = Axon::<String, String, String>::new("ExportTest")
            .then(SchemaTransition)
            .with_output_schema_value(serde_json::json!({"type": "string"}));

        let json = serde_json::to_value(&axon.schematic).unwrap();
        let nodes = json["nodes"].as_array().unwrap();
        // Find the SchemaTransition node (last one)
        let last = nodes.last().unwrap();
        assert!(last.get("input_schema").is_some());
        assert_eq!(last["input_schema"]["type"], "object");
        assert_eq!(last["output_schema"]["type"], "string");
    }

    #[test]
    fn schematic_export_omits_schema_fields_when_none() {
        let axon = Axon::<i32, i32, TestInfallible>::new("NoSchemaExport").then(AddOne);

        let json = serde_json::to_value(&axon.schematic).unwrap();
        let nodes = json["nodes"].as_array().unwrap();
        let last = nodes.last().unwrap();
        let obj = last.as_object().unwrap();
        assert!(!obj.contains_key("input_schema"));
        assert!(!obj.contains_key("output_schema"));
    }

    #[test]
    fn schematic_json_roundtrip_preserves_schemas() {
        let axon = Axon::<String, String, String>::new("Roundtrip")
            .then(SchemaTransition)
            .with_output_schema_value(serde_json::json!({"type": "string"}));

        let json_str = serde_json::to_string(&axon.schematic).unwrap();
        let deserialized: ranvier_core::schematic::Schematic =
            serde_json::from_str(&json_str).unwrap();

        let last = deserialized.nodes.last().unwrap();
        assert!(last.input_schema.is_some());
        assert!(last.output_schema.is_some());
        assert_eq!(last.input_schema.as_ref().unwrap()["required"][0], "name");
        assert_eq!(last.output_schema.as_ref().unwrap()["type"], "string");
    }

    // Test transitions for new unit tests
    #[derive(Clone)]
    struct MultiplyByTwo;

    #[async_trait]
    impl Transition<i32, i32> for MultiplyByTwo {
        type Error = TestInfallible;
        type Resources = ();

        async fn run(
            &self,
            state: i32,
            _resources: &Self::Resources,
            _bus: &mut Bus,
        ) -> Outcome<i32, Self::Error> {
            Outcome::Next(state * 2)
        }
    }

    #[derive(Clone)]
    struct AddTen;

    #[async_trait]
    impl Transition<i32, i32> for AddTen {
        type Error = TestInfallible;
        type Resources = ();

        async fn run(
            &self,
            state: i32,
            _resources: &Self::Resources,
            _bus: &mut Bus,
        ) -> Outcome<i32, Self::Error> {
            Outcome::Next(state + 10)
        }
    }

    #[derive(Clone)]
    struct AddOneString;

    #[async_trait]
    impl Transition<i32, i32> for AddOneString {
        type Error = String;
        type Resources = ();

        async fn run(
            &self,
            state: i32,
            _resources: &Self::Resources,
            _bus: &mut Bus,
        ) -> Outcome<i32, Self::Error> {
            Outcome::Next(state + 1)
        }
    }

    #[derive(Clone)]
    struct AddTenString;

    #[async_trait]
    impl Transition<i32, i32> for AddTenString {
        type Error = String;
        type Resources = ();

        async fn run(
            &self,
            state: i32,
            _resources: &Self::Resources,
            _bus: &mut Bus,
        ) -> Outcome<i32, Self::Error> {
            Outcome::Next(state + 10)
        }
    }

    #[tokio::test]
    async fn axon_single_step_chain_executes_and_returns_next() {
        let mut bus = Bus::new();
        let axon = Axon::<i32, i32, TestInfallible>::start("SingleStep").then(AddOne);

        let outcome = axon.execute(5, &(), &mut bus).await;
        assert!(matches!(outcome, Outcome::Next(6)));
    }

    #[tokio::test]
    async fn axon_three_step_chain_executes_in_order() {
        let mut bus = Bus::new();
        let axon = Axon::<i32, i32, TestInfallible>::start("ThreeStep")
            .then(AddOne)
            .then(MultiplyByTwo)
            .then(AddTen);

        // Starting with 5: AddOne -> 6, MultiplyByTwo -> 12, AddTen -> 22
        let outcome = axon.execute(5, &(), &mut bus).await;
        assert!(matches!(outcome, Outcome::Next(22)));
    }

    #[tokio::test]
    async fn axon_with_fault_in_middle_step_propagates_error() {
        let mut bus = Bus::new();

        // Create a 3-step chain where the middle step faults
        // Step 1: AddOneString (5 -> 6)
        // Step 2: AlwaysFault (should fault here)
        // Step 3: AddTenString (never reached)
        let axon = Axon::<i32, i32, String>::start("FaultInMiddle")
            .then(AddOneString)
            .then(AlwaysFault)
            .then(AddTenString);

        let outcome = axon.execute(5, &(), &mut bus).await;
        assert!(matches!(outcome, Outcome::Fault(msg) if msg == "boom"));
    }

    #[tokio::test]
    async fn fault_injects_transition_error_context_into_bus() {
        let mut bus = Bus::new();

        // 3-step chain: AddOneString → AlwaysFault → AddTenString
        let axon = Axon::<i32, i32, String>::start("my-pipeline")
            .then(AddOneString)
            .then(AlwaysFault)
            .then(AddTenString);

        let outcome = axon.execute(5, &(), &mut bus).await;
        assert!(matches!(outcome, Outcome::Fault(_)));

        let ctx = bus
            .read::<ranvier_core::error::TransitionErrorContext>()
            .expect("TransitionErrorContext should be in Bus after fault");
        assert_eq!(ctx.pipeline_name, "my-pipeline");
        assert_eq!(ctx.transition_name, "AlwaysFault");
        assert_eq!(ctx.step_index, 2); // 0=ingress, 1=AddOneString, 2=AlwaysFault
    }

    #[test]
    fn axon_schematic_has_correct_node_count_after_chaining() {
        let axon = Axon::<i32, i32, TestInfallible>::start("NodeCount")
            .then(AddOne)
            .then(MultiplyByTwo)
            .then(AddTen);

        // Should have 4 nodes: ingress + 3 transitions
        assert_eq!(axon.schematic.nodes.len(), 4);
        assert_eq!(axon.schematic.name, "NodeCount");
    }

    #[tokio::test]
    async fn axon_execution_records_timeline_events() {
        let mut bus = Bus::new();
        bus.insert(Timeline::new());

        let axon = Axon::<i32, i32, TestInfallible>::start("TimelineTest")
            .then(AddOne)
            .then(MultiplyByTwo);

        let outcome = axon.execute(3, &(), &mut bus).await;
        assert!(matches!(outcome, Outcome::Next(8))); // (3 + 1) * 2 = 8

        let timeline = bus.read::<Timeline>().unwrap();

        // Should have NodeEnter and NodeExit events
        let enter_count = timeline
            .events
            .iter()
            .filter(|e| matches!(e, TimelineEvent::NodeEnter { .. }))
            .count();
        let exit_count = timeline
            .events
            .iter()
            .filter(|e| matches!(e, TimelineEvent::NodeExit { .. }))
            .count();

        // Expect at least 1 enter and 1 exit (ingress node)
        assert!(enter_count >= 1, "Should have at least 1 NodeEnter event");
        assert!(exit_count >= 1, "Should have at least 1 NodeExit event");
    }

    // ── Parallel Step Tests (M231) ───────────────────────────────

    #[tokio::test]
    async fn parallel_all_succeed_returns_first_next() {
        use super::ParallelStrategy;

        let mut bus = Bus::new();
        let axon = Axon::<i32, i32, TestInfallible>::start("ParallelAllSucceed")
            .parallel(
                vec![
                    Arc::new(AddOne) as Arc<dyn Transition<i32, i32, Resources = (), Error = TestInfallible> + Send + Sync>,
                    Arc::new(MultiplyByTwo),
                ],
                ParallelStrategy::AllMustSucceed,
            );

        // Input 5: AddOne -> 6, MultiplyByTwo -> 10.
        // AllMustSucceed returns the first Next (AddOne = 6).
        let outcome = axon.execute(5, &(), &mut bus).await;
        assert!(matches!(outcome, Outcome::Next(6)));
    }

    #[tokio::test]
    async fn parallel_all_must_succeed_returns_fault_when_any_fails() {
        use super::ParallelStrategy;

        let mut bus = Bus::new();
        let axon = Axon::<i32, i32, String>::start("ParallelAllFault")
            .parallel(
                vec![
                    Arc::new(AddOneString) as Arc<dyn Transition<i32, i32, Resources = (), Error = String> + Send + Sync>,
                    Arc::new(AlwaysFault),
                ],
                ParallelStrategy::AllMustSucceed,
            );

        let outcome = axon.execute(5, &(), &mut bus).await;
        assert!(
            matches!(outcome, Outcome::Fault(ref msg) if msg == "boom"),
            "Expected Fault(boom), got {:?}",
            outcome
        );
    }

    #[tokio::test]
    async fn parallel_any_can_fail_returns_success_despite_fault() {
        use super::ParallelStrategy;

        let mut bus = Bus::new();
        let axon = Axon::<i32, i32, String>::start("ParallelAnyCanFail")
            .parallel(
                vec![
                    Arc::new(AlwaysFault) as Arc<dyn Transition<i32, i32, Resources = (), Error = String> + Send + Sync>,
                    Arc::new(AddOneString),
                ],
                ParallelStrategy::AnyCanFail,
            );

        // AlwaysFault faults, but AddOneString succeeds (5 + 1 = 6).
        let outcome = axon.execute(5, &(), &mut bus).await;
        assert!(
            matches!(outcome, Outcome::Next(6)),
            "Expected Next(6), got {:?}",
            outcome
        );
    }

    #[tokio::test]
    async fn parallel_any_can_fail_all_fault_returns_first_fault() {
        use super::ParallelStrategy;

        #[derive(Clone)]
        struct AlwaysFault2;
        #[async_trait]
        impl Transition<i32, i32> for AlwaysFault2 {
            type Error = String;
            type Resources = ();
            async fn run(&self, _state: i32, _resources: &(), _bus: &mut Bus) -> Outcome<i32, String> {
                Outcome::Fault("boom2".to_string())
            }
        }

        let mut bus = Bus::new();
        let axon = Axon::<i32, i32, String>::start("ParallelAllFault2")
            .parallel(
                vec![
                    Arc::new(AlwaysFault) as Arc<dyn Transition<i32, i32, Resources = (), Error = String> + Send + Sync>,
                    Arc::new(AlwaysFault2),
                ],
                ParallelStrategy::AnyCanFail,
            );

        let outcome = axon.execute(5, &(), &mut bus).await;
        // Should return the first fault
        assert!(
            matches!(outcome, Outcome::Fault(ref msg) if msg == "boom"),
            "Expected Fault(boom), got {:?}",
            outcome
        );
    }

    #[test]
    fn parallel_schematic_has_fanout_fanin_nodes() {
        use super::ParallelStrategy;
        use ranvier_core::schematic::{EdgeType, NodeKind};

        let axon = Axon::<i32, i32, TestInfallible>::start("ParallelSchematic")
            .parallel(
                vec![
                    Arc::new(AddOne) as Arc<dyn Transition<i32, i32, Resources = (), Error = TestInfallible> + Send + Sync>,
                    Arc::new(MultiplyByTwo),
                    Arc::new(AddTen),
                ],
                ParallelStrategy::AllMustSucceed,
            );

        // Should have: Ingress + FanOut + 3 branch Atoms + FanIn = 6 nodes
        assert_eq!(axon.schematic.nodes.len(), 6);
        assert!(matches!(axon.schematic.nodes[1].kind, NodeKind::FanOut));
        assert!(matches!(axon.schematic.nodes[2].kind, NodeKind::Atom));
        assert!(matches!(axon.schematic.nodes[3].kind, NodeKind::Atom));
        assert!(matches!(axon.schematic.nodes[4].kind, NodeKind::Atom));
        assert!(matches!(axon.schematic.nodes[5].kind, NodeKind::FanIn));

        // Check FanOut description
        assert!(axon.schematic.nodes[1]
            .description
            .as_ref()
            .unwrap()
            .contains("3 branches"));

        // Check parallel edges from FanOut to branches
        let parallel_edges: Vec<_> = axon
            .schematic
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeType::Parallel))
            .collect();
        // 3 from FanOut->branches + 3 from branches->FanIn = 6
        assert_eq!(parallel_edges.len(), 6);
    }

    #[tokio::test]
    async fn parallel_then_chain_composes_correctly() {
        use super::ParallelStrategy;

        let mut bus = Bus::new();
        let axon = Axon::<i32, i32, TestInfallible>::start("ParallelThenChain")
            .then(AddOne)
            .parallel(
                vec![
                    Arc::new(AddOne) as Arc<dyn Transition<i32, i32, Resources = (), Error = TestInfallible> + Send + Sync>,
                    Arc::new(MultiplyByTwo),
                ],
                ParallelStrategy::AllMustSucceed,
            )
            .then(AddTen);

        // 5 -> AddOne -> 6 -> Parallel(AddOne=7, MultiplyByTwo=12) -> first=7 -> AddTen -> 17
        let outcome = axon.execute(5, &(), &mut bus).await;
        assert!(
            matches!(outcome, Outcome::Next(17)),
            "Expected Next(17), got {:?}",
            outcome
        );
    }

    #[tokio::test]
    async fn parallel_records_timeline_events() {
        use super::ParallelStrategy;
        use ranvier_core::timeline::TimelineEvent;

        let mut bus = Bus::new();
        bus.insert(Timeline::new());

        let axon = Axon::<i32, i32, TestInfallible>::start("ParallelTimeline")
            .parallel(
                vec![
                    Arc::new(AddOne) as Arc<dyn Transition<i32, i32, Resources = (), Error = TestInfallible> + Send + Sync>,
                    Arc::new(MultiplyByTwo),
                ],
                ParallelStrategy::AllMustSucceed,
            );

        let outcome = axon.execute(3, &(), &mut bus).await;
        assert!(matches!(outcome, Outcome::Next(4)));

        let timeline = bus.read::<Timeline>().unwrap();

        // Check for FanOut enter/exit and FanIn enter/exit
        let fanout_enters = timeline
            .events
            .iter()
            .filter(|e| matches!(e, TimelineEvent::NodeEnter { node_label, .. } if node_label == "FanOut"))
            .count();
        let fanin_enters = timeline
            .events
            .iter()
            .filter(|e| matches!(e, TimelineEvent::NodeEnter { node_label, .. } if node_label == "FanIn"))
            .count();

        assert_eq!(fanout_enters, 1, "Should have 1 FanOut enter");
        assert_eq!(fanin_enters, 1, "Should have 1 FanIn enter");
    }

    // ── Axon::simple() convenience constructor ───────────────────────────────

    #[derive(Clone)]
    struct Greet;

    #[async_trait]
    impl Transition<(), String> for Greet {
        type Error = String;
        type Resources = ();

        async fn run(
            &self,
            _state: (),
            _resources: &Self::Resources,
            _bus: &mut Bus,
        ) -> Outcome<String, Self::Error> {
            Outcome::Next("Hello from simple!".to_string())
        }
    }

    #[tokio::test]
    async fn axon_simple_creates_pipeline() {
        let axon = Axon::simple::<String>("SimpleTest").then(Greet);

        let mut bus = Bus::new();
        let result = axon.execute((), &(), &mut bus).await;

        match result {
            Outcome::Next(msg) => assert_eq!(msg, "Hello from simple!"),
            other => panic!("Expected Outcome::Next, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn axon_simple_equivalent_to_explicit() {
        // Axon::simple::<E>("label") should behave identically to Axon::<(), (), E>::new("label")
        let simple = Axon::simple::<String>("Equiv").then(Greet);
        let explicit = Axon::<(), (), String>::new("Equiv").then(Greet);

        let mut bus1 = Bus::new();
        let mut bus2 = Bus::new();

        let r1 = simple.execute((), &(), &mut bus1).await;
        let r2 = explicit.execute((), &(), &mut bus2).await;

        match (r1, r2) {
            (Outcome::Next(a), Outcome::Next(b)) => assert_eq!(a, b),
            _ => panic!("Both should produce Outcome::Next"),
        }
    }

    #[tokio::test]
    async fn then_fn_closure_transition() {
        let axon = Axon::simple::<String>("ClosureTest")
            .then_fn("to_greeting", |_input: (), _bus: &mut Bus| {
                Outcome::next("hello from closure".to_string())
            });

        let mut bus = Bus::new();
        let result = axon.execute((), &(), &mut bus).await;

        match result {
            Outcome::Next(msg) => assert_eq!(msg, "hello from closure"),
            other => panic!("Expected Outcome::Next, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn then_fn_reads_bus() {
        let axon = Axon::simple::<String>("BusReadClosure")
            .then_fn("check_score", |_input: (), bus: &mut Bus| {
                let score = bus.read::<u32>().copied().unwrap_or(0);
                if score > 75 {
                    Outcome::next("REJECTED".to_string())
                } else {
                    Outcome::next("APPROVED".to_string())
                }
            });

        let mut bus = Bus::new();
        bus.insert(80u32);
        let result = axon.execute((), &(), &mut bus).await;
        match result {
            Outcome::Next(msg) => assert_eq!(msg, "REJECTED"),
            other => panic!("Expected REJECTED, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn then_fn_mixed_with_transition() {
        // Closure and macro Transition in the same chain
        let axon = Axon::simple::<String>("MixedPipeline")
            .then(Greet)
            .then_fn("uppercase", |input: String, _bus: &mut Bus| {
                Outcome::next(input.to_uppercase())
            });

        let mut bus = Bus::new();
        let result = axon.execute((), &(), &mut bus).await;
        match result {
            Outcome::Next(msg) => assert_eq!(msg, "HELLO FROM SIMPLE!"),
            other => panic!("Expected uppercase greeting, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn then_fn_schematic_label() {
        let axon = Axon::simple::<String>("SchematicTest")
            .then_fn("my_custom_label", |_: (), _: &mut Bus| {
                Outcome::next("ok".to_string())
            });

        // Node 0 is the identity start node, node 1 is our closure
        assert_eq!(axon.schematic.nodes.len(), 2);
        assert_eq!(axon.schematic.nodes[1].label, "my_custom_label");
    }
}
