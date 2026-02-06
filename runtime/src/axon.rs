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

use ranvier_core::bus::Bus;
use ranvier_core::outcome::Outcome;
use ranvier_core::schematic::{Edge, EdgeType, Node, NodeKind, Schematic};
use ranvier_core::timeline::{Timeline, TimelineEvent};
use ranvier_core::transition::Transition;
use std::fs;
use std::any::type_name;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::Instrument;

/// Type alias for async boxed futures used in Axon execution.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Executor type for Axon steps.
/// Now takes an input state `In`, a resource bundle `Res`, and returns an `Outcome<Out, E>`.
/// Must be Send + Sync to be reusable across threads and clones.
pub type Executor<In, Out, E, Res> =
    Arc<dyn for<'a> Fn(In, &'a Res, &'a mut Bus) -> BoxFuture<'a, Outcome<Out, E>> + Send + Sync>;

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
    executor: Executor<In, Out, E, Res>,
}

impl<In, Out, E, Res> Clone for Axon<In, Out, E, Res> {
    fn clone(&self) -> Self {
        Self {
            schematic: self.schematic.clone(),
            executor: self.executor.clone(),
        }
    }
}

impl<In, E, Res> Axon<In, In, E, Res>
where
    In: Send + Sync + 'static,
    E: Send + 'static,
    Res: ranvier_core::transition::ResourceRequirement,
{
    /// Create a new Axon flow with the given label.
    /// This is the preferred entry point per Flat API guidelines.
    pub fn new(label: &str) -> Self {
        Self::start(label)
    }

    /// Start defining a new Axon flow.
    /// This creates an Identity Axon (In -> In) with no initial resource requirements.
    pub fn start(label: &str) -> Self {
        let node_id = uuid::Uuid::new_v4().to_string();
        let node = Node {
            id: node_id,
            kind: NodeKind::Ingress,
            label: label.to_string(),
            description: None,
            input_type: "void".to_string(),
            output_type: type_name_of::<In>(),
            resource_type: type_name_of::<Res>(),
            metadata: Default::default(),
            source_location: None,
        };

        let mut schematic = Schematic::new(label);
        schematic.nodes.push(node);

        let executor: Executor<In, In, E, Res> =
            Arc::new(move |input, _res, _bus| Box::pin(std::future::ready(Outcome::Next(input))));

        Self {
            schematic,
            executor,
        }
    }
}

impl<In, Out, E, Res> Axon<In, Out, E, Res>
where
    In: Send + Sync + 'static,
    Out: Send + Sync + 'static,
    E: Send + 'static,
    Res: ranvier_core::transition::ResourceRequirement,
{
    /// Chain a transition to this Axon.
    ///
    /// Requires the transition to use the SAME resource bundle as the previous steps.
    pub fn then<Next, Trans>(self, transition: Trans) -> Axon<In, Next, E, Res>
    where
        Next: Send + Sync + 'static,
        Trans: Transition<Out, Next, Resources = Res, Error = E> + Clone + Send + Sync + 'static,
    {
        // Decompose self to avoid partial move issues
        let Axon {
            mut schematic,
            executor: prev_executor,
        } = self;

        // Update Schematic
        let next_node_id = uuid::Uuid::new_v4().to_string();
        let next_node = Node {
            id: next_node_id.clone(),
            kind: NodeKind::Atom,
            label: transition.label(),
            description: transition.description(),
            input_type: type_name_of::<Out>(),
            output_type: type_name_of::<Next>(),
            resource_type: type_name_of::<Res>(),
            metadata: Default::default(),
            source_location: None,
        };

        let last_node_id = schematic
            .nodes
            .last()
            .map(|n| n.id.clone())
            .unwrap_or_default();

        schematic.nodes.push(next_node);
        schematic.edges.push(Edge {
            from: last_node_id,
            to: next_node_id.clone(),
            kind: EdgeType::Linear,
            label: Some("Next".to_string()),
        });

        // Compose Executor
        let node_id_for_exec = next_node_id.clone();
        let node_label_for_exec = transition.label();
        let next_executor: Executor<In, Next, E, Res> = Arc::new(
            move |input: In, res: &Res, bus: &mut Bus| -> BoxFuture<'_, Outcome<Next, E>> {
                let prev = prev_executor.clone();
                let trans = transition.clone();
                let timeline_node_id = node_id_for_exec.clone();
                let timeline_node_label = node_label_for_exec.clone();

                Box::pin(async move {
                    // Run previous step
                    let prev_result = prev(input, res, bus).await;

                    // Unpack
                    let state = match prev_result {
                        Outcome::Next(t) => t,
                        other => return other.map(|_| unreachable!()),
                    };

                    // Run this step with automatic instrumentation
                    let label = trans.label();
                    let res_type = std::any::type_name::<Res>()
                        .split("::")
                        .last()
                        .unwrap_or("unknown");

                    let enter_ts = now_ms();
                    if let Some(timeline) = bus.read_mut::<Timeline>() {
                        timeline.push(TimelineEvent::NodeEnter {
                            node_id: timeline_node_id.clone(),
                            node_label: timeline_node_label.clone(),
                            timestamp: enter_ts,
                        });
                    }

                    let started = std::time::Instant::now();
                    let result = trans
                        .run(state, res, bus)
                        .instrument(tracing::info_span!(
                            "Node",
                            ranvier.node = %label,
                            ranvier.resource_type = %res_type
                        ))
                        .await;
                    let duration_ms = started.elapsed().as_millis() as u64;
                    let exit_ts = now_ms();

                    if let Some(timeline) = bus.read_mut::<Timeline>() {
                        timeline.push(TimelineEvent::NodeExit {
                            node_id: timeline_node_id.clone(),
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

                    result
                })
            },
        );

        Axon {
            schematic,
            executor: next_executor,
        }
    }

    /// Add a branch point
    pub fn branch(mut self, branch_id: impl Into<String>, label: &str) -> Self {
        let branch_id_str = branch_id.into();
        let last_node_id = self
            .schematic
            .nodes
            .last()
            .map(|n| n.id.clone())
            .unwrap_or_default();

        let branch_node = Node {
            id: uuid::Uuid::new_v4().to_string(),
            kind: NodeKind::Synapse,
            label: label.to_string(),
            description: None,
            input_type: type_name_of::<Out>(),
            output_type: type_name_of::<Out>(),
            resource_type: type_name_of::<Res>(),
            metadata: Default::default(),
            source_location: None,
        };

        self.schematic.nodes.push(branch_node);
        self.schematic.edges.push(Edge {
            from: last_node_id,
            to: branch_id_str.clone(),
            kind: EdgeType::Branch(branch_id_str),
            label: Some("Branch".to_string()),
        });

        self
    }

    /// Execute the Axon with the given input and resources.
    pub async fn execute(&self, input: In, resources: &Res, bus: &mut Bus) -> Outcome<Out, E> {
        let should_capture = should_attach_timeline(bus);
        let inserted_timeline = if should_capture {
            ensure_timeline(bus)
        } else {
            false
        };
        let ingress_started = std::time::Instant::now();
        let ingress_enter_ts = now_ms();
        if should_capture
            && let (Some(timeline), Some(ingress)) =
            (bus.read_mut::<Timeline>(), self.schematic.nodes.first())
        {
            timeline.push(TimelineEvent::NodeEnter {
                node_id: ingress.id.clone(),
                node_label: ingress.label.clone(),
                timestamp: ingress_enter_ts,
            });
        }

        let label = self.schematic.name.clone();
        let outcome = (self.executor)(input, resources, bus)
            .instrument(tracing::info_span!("Circuit", ranvier.circuit = %label))
            .await;

        let ingress_exit_ts = now_ms();
        if should_capture
            && let (Some(timeline), Some(ingress)) =
            (bus.read_mut::<Timeline>(), self.schematic.nodes.first())
        {
            timeline.push(TimelineEvent::NodeExit {
                node_id: ingress.id.clone(),
                outcome_type: outcome_type_name(&outcome),
                duration_ms: ingress_started.elapsed().as_millis() as u64,
                timestamp: ingress_exit_ts,
            });
        }

        if should_capture {
            maybe_export_timeline(bus, &outcome);
        }
        if inserted_timeline {
            let _ = bus.remove::<Timeline>();
        }

        outcome
    }

    /// Starts the Ranvier Inspector for this Axon on the specified port.
    /// This spawns a background task to serve the Schematic.
    pub fn serve_inspector(self, port: u16) -> Self {
        if !inspector_enabled_from_env() {
            tracing::info!("Inspector disabled by RANVIER_INSPECTOR");
            return self;
        }

        let schematic = self.schematic.clone();
        tokio::spawn(async move {
            if let Err(e) = ranvier_inspector::Inspector::new(schematic, port)
                .with_projection_files_from_env()
                .with_mode_from_env()
                .serve()
                .await
            {
                tracing::error!("Inspector server failed: {}", e);
            }
        });
        self
    }

    /// Get a reference to the Schematic (structural view).
    pub fn schematic(&self) -> &Schematic {
        &self.schematic
    }

    /// Consume and return the Schematic.
    pub fn into_schematic(self) -> Schematic {
        self.schematic
    }
}

fn inspector_enabled_from_env() -> bool {
    match std::env::var("RANVIER_INSPECTOR") {
        Ok(v) => matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "on" | "yes"),
        Err(_) => true,
    }
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

fn outcome_type_name<Out, E>(outcome: &Outcome<Out, E>) -> String {
    match outcome {
        Outcome::Next(_) => "Next".to_string(),
        Outcome::Branch(id, _) => format!("Branch:{}", id),
        Outcome::Jump(id, _) => format!("Jump:{}", id),
        Outcome::Emit(event_type, _) => format!("Emit:{}", event_type),
        Outcome::Fault(_) => "Fault".to_string(),
    }
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

fn write_timeline_with_policy(path: &str, mode: &str, mut timeline: Timeline) -> Result<(), String> {
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
    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    let json = serde_json::to_string_pretty(timeline).map_err(|e| e.to_string())?;
    fs::write(path, json).map_err(|e| e.to_string())
}

fn rotated_path(path: &str, suffix: u64) -> PathBuf {
    let p = Path::new(path);
    let parent = p.parent().unwrap_or_else(|| Path::new(""));
    let stem = p
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("timeline");
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
    let stem = p
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("timeline");
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
        .filter_map(|entry| {
            let modified = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            Some((entry.path(), modified))
        })
        .collect::<Vec<_>>();

    files.sort_by(|a, b| b.1.cmp(&a.1));
    for (path, _) in files.into_iter().skip(keep) {
        let _ = fs::remove_file(path);
    }
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
