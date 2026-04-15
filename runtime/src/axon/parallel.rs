use ranvier_core::bus::Bus;
use ranvier_core::outcome::Outcome;
use ranvier_core::schematic::{Edge, EdgeType, Node, NodeKind, SourceLocation};
use ranvier_core::timeline::{Timeline, TimelineEvent};
use ranvier_core::transition::Transition;
use serde::{Serialize, de::DeserializeOwned};
use std::panic::Location;
use std::sync::Arc;

use crate::persistence::PersistenceHandle;

use super::*;
use super::{
    bus_capability_schema_from_policy, now_ms, outcome_kind_name, outcome_type_name,
    persist_execution_event, persistence_trace_id, type_name_of,
};

impl<In, Out, E, Res> Axon<In, Out, E, Res>
where
    In: Send + Sync + Serialize + DeserializeOwned + 'static,
    Out: Send + Sync + Serialize + DeserializeOwned + 'static,
    E: Send + Sync + Serialize + DeserializeOwned + std::fmt::Debug + 'static,
    Res: ranvier_core::transition::ResourceRequirement,
{
    /// Each transition receives a clone of the current step's output and runs
    /// concurrently via [`futures_util::future::join_all`]. The strategy controls
    /// how faults are handled:
    ///
    /// - [`ParallelStrategy::AllMustSucceed`]: All branches must produce `Next`.
    ///   If any branch returns `Fault`, the first fault is propagated.
    /// - [`ParallelStrategy::AnyCanFail`]: Branches that fault are ignored as
    ///   long as at least one succeeds. If all branches fault, the first fault
    ///   is returned.
    ///
    /// The **first successful `Next` value** is forwarded to the next step in
    /// the pipeline. A custom merge can be layered on top via a subsequent
    /// `.then()` step.
    ///
    /// Each parallel branch receives its own fresh [`Bus`] instance. Resources
    /// should be injected via the shared `Res` bundle rather than the Bus for
    /// parallel steps.
    ///
    /// ## Schematic
    ///
    /// The method emits a `FanOut` node, one `Atom` node per branch (connected
    /// via `Parallel` edges), and a `FanIn` join node.
    ///
    /// ## Example
    ///
    /// ```rust,ignore
    /// let axon = Axon::new("Pipeline")
    ///     .then(ParseInput)
    ///     .parallel(
    ///         vec![Arc::new(EnrichA), Arc::new(EnrichB)],
    ///         ParallelStrategy::AllMustSucceed,
    ///     )
    ///     .then(MergeResults);
    /// ```
    #[track_caller]
    pub fn parallel(
        self,
        transitions: Vec<Arc<dyn Transition<Out, Out, Resources = Res, Error = E> + Send + Sync>>,
        strategy: ParallelStrategy,
    ) -> Axon<In, Out, E, Res>
    where
        Out: Clone,
    {
        let caller = Location::caller();
        let Axon {
            mut schematic,
            executor: prev_executor,
            execution_mode,
            persistence_store,
            audit_sink,
            dlq_sink,
            dlq_policy,
            dynamic_dlq_policy,
            saga_policy,
            dynamic_saga_policy,
            saga_compensation_registry,
            iam_handle,
        } = self;

        // ── Schematic: FanOut node ─────────────────────────────────
        let fanout_id = uuid::Uuid::new_v4().to_string();
        let fanin_id = uuid::Uuid::new_v4().to_string();

        let last_node_id = schematic
            .nodes
            .last()
            .map(|n| n.id.clone())
            .unwrap_or_default();

        let fanout_node = Node {
            id: fanout_id.clone(),
            kind: NodeKind::FanOut,
            label: "FanOut".to_string(),
            description: Some(format!(
                "Parallel split ({} branches, {:?})",
                transitions.len(),
                strategy
            )),
            input_type: type_name_of::<Out>(),
            output_type: type_name_of::<Out>(),
            resource_type: type_name_of::<Res>(),
            metadata: Default::default(),
            bus_capability: None,
            source_location: Some(SourceLocation::new(caller.file(), caller.line())),
            position: None,
            compensation_node_id: None,
            input_schema: None,
            output_schema: None,
            item_type: None,
            terminal: None,
        };

        schematic.nodes.push(fanout_node);
        schematic.edges.push(Edge {
            from: last_node_id,
            to: fanout_id.clone(),
            kind: EdgeType::Linear,
            label: Some("Next".to_string()),
        });

        // ── Schematic: one Atom node per parallel branch ───────────
        let mut branch_node_ids = Vec::with_capacity(transitions.len());
        for (i, trans) in transitions.iter().enumerate() {
            let branch_id = uuid::Uuid::new_v4().to_string();
            let branch_node = Node {
                id: branch_id.clone(),
                kind: NodeKind::Atom,
                label: trans.label(),
                description: trans.description(),
                input_type: type_name_of::<Out>(),
                output_type: type_name_of::<Out>(),
                resource_type: type_name_of::<Res>(),
                metadata: Default::default(),
                bus_capability: bus_capability_schema_from_policy(trans.bus_access_policy()),
                source_location: Some(SourceLocation::new(caller.file(), caller.line())),
                position: None,
                compensation_node_id: None,
                input_schema: trans.input_schema(),
                output_schema: None,
                item_type: None,
                terminal: None,
            };
            schematic.nodes.push(branch_node);
            schematic.edges.push(Edge {
                from: fanout_id.clone(),
                to: branch_id.clone(),
                kind: EdgeType::Parallel,
                label: Some(format!("Branch {}", i)),
            });
            branch_node_ids.push(branch_id);
        }

        // ── Schematic: FanIn node ──────────────────────────────────
        let fanin_node = Node {
            id: fanin_id.clone(),
            kind: NodeKind::FanIn,
            label: "FanIn".to_string(),
            description: Some(format!("Parallel join ({:?})", strategy)),
            input_type: type_name_of::<Out>(),
            output_type: type_name_of::<Out>(),
            resource_type: type_name_of::<Res>(),
            metadata: Default::default(),
            bus_capability: None,
            source_location: Some(SourceLocation::new(caller.file(), caller.line())),
            position: None,
            compensation_node_id: None,
            input_schema: None,
            output_schema: None,
            item_type: None,
            terminal: None,
        };

        schematic.nodes.push(fanin_node);
        for branch_id in &branch_node_ids {
            schematic.edges.push(Edge {
                from: branch_id.clone(),
                to: fanin_id.clone(),
                kind: EdgeType::Parallel,
                label: Some("Join".to_string()),
            });
        }

        // ── Executor: parallel composition ─────────────────────────
        let fanout_node_id = fanout_id.clone();
        let fanin_node_id = fanin_id.clone();
        let branch_ids_for_exec = branch_node_ids.clone();
        let step_idx = schematic.nodes.len() as u64 - 1;

        let next_executor: Executor<In, Out, E, Res> = Arc::new(
            move |input: In, res: &Res, bus: &mut Bus| -> BoxFuture<'_, Outcome<Out, E>> {
                let prev = prev_executor.clone();
                let branches = transitions.clone();
                let fanout_id = fanout_node_id.clone();
                let fanin_id = fanin_node_id.clone();
                let branch_ids = branch_ids_for_exec.clone();

                Box::pin(async move {
                    // Run previous steps
                    let prev_result = prev(input, res, bus).await;
                    let state = match prev_result {
                        Outcome::Next(t) => t,
                        other => return other.map(|_| unreachable!()),
                    };

                    // Timeline: FanOut enter
                    let fanout_enter_ts = now_ms();
                    if let Some(timeline) = bus.read_mut::<Timeline>() {
                        timeline.push(TimelineEvent::NodeEnter {
                            node_id: fanout_id.clone(),
                            node_label: "FanOut".to_string(),
                            timestamp: fanout_enter_ts,
                        });
                    }

                    // Build futures for all branches.
                    // Each branch gets its own Bus so they can run concurrently
                    // without &mut Bus aliasing. Resources are shared via &Res.
                    let futs: Vec<_> = branches
                        .iter()
                        .enumerate()
                        .map(|(i, trans)| {
                            let branch_state = state.clone();
                            let branch_node_id = branch_ids[i].clone();
                            let trans = trans.clone();

                            async move {
                                let mut branch_bus = Bus::new();
                                let label = trans.label();
                                let bus_policy = trans.bus_access_policy();

                                branch_bus.set_access_policy(label.clone(), bus_policy);
                                let result = trans.run(branch_state, res, &mut branch_bus).await;
                                branch_bus.clear_access_policy();

                                (i, branch_node_id, label, result)
                            }
                        })
                        .collect();

                    // Run all branches concurrently within the current task
                    let results: Vec<(usize, String, String, Outcome<Out, E>)> =
                        futures_util::future::join_all(futs).await;

                    // Timeline: record each branch's enter/exit
                    for (_, branch_node_id, branch_label, outcome) in &results {
                        if let Some(timeline) = bus.read_mut::<Timeline>() {
                            let ts = now_ms();
                            timeline.push(TimelineEvent::NodeEnter {
                                node_id: branch_node_id.clone(),
                                node_label: branch_label.clone(),
                                timestamp: ts,
                            });
                            timeline.push(TimelineEvent::NodeExit {
                                node_id: branch_node_id.clone(),
                                outcome_type: outcome_type_name(outcome),
                                duration_ms: 0,
                                timestamp: ts,
                            });
                        }
                    }

                    // Timeline: FanOut exit
                    if let Some(timeline) = bus.read_mut::<Timeline>() {
                        timeline.push(TimelineEvent::NodeExit {
                            node_id: fanout_id.clone(),
                            outcome_type: "Next".to_string(),
                            duration_ms: 0,
                            timestamp: now_ms(),
                        });
                    }

                    // ── Apply strategy ─────────────────────────────
                    let combined = match strategy {
                        ParallelStrategy::AllMustSucceed => {
                            let mut first_fault = None;
                            let mut first_success = None;

                            for (_, _, _, outcome) in results {
                                match outcome {
                                    Outcome::Fault(e) => {
                                        if first_fault.is_none() {
                                            first_fault = Some(Outcome::Fault(e));
                                        }
                                    }
                                    Outcome::Next(val) => {
                                        if first_success.is_none() {
                                            first_success = Some(Outcome::Next(val));
                                        }
                                    }
                                    other => {
                                        // Non-Next/non-Fault outcomes (Branch, Emit, Jump)
                                        // treated as non-success in AllMustSucceed
                                        if first_fault.is_none() {
                                            first_fault = Some(other);
                                        }
                                    }
                                }
                            }

                            if let Some(fault) = first_fault {
                                fault
                            } else {
                                first_success.unwrap_or_else(|| {
                                    Outcome::emit("execution.parallel.no_results", None)
                                })
                            }
                        }
                        ParallelStrategy::AnyCanFail => {
                            let mut first_success = None;
                            let mut first_fault = None;

                            for (_, _, _, outcome) in results {
                                match outcome {
                                    Outcome::Next(val) => {
                                        if first_success.is_none() {
                                            first_success = Some(Outcome::Next(val));
                                        }
                                    }
                                    Outcome::Fault(e) => {
                                        if first_fault.is_none() {
                                            first_fault = Some(Outcome::Fault(e));
                                        }
                                    }
                                    _ => {}
                                }
                            }

                            first_success.unwrap_or_else(|| {
                                first_fault.unwrap_or_else(|| {
                                    Outcome::emit("execution.parallel.no_results", None)
                                })
                            })
                        }
                    };

                    // Timeline: FanIn
                    let fanin_enter_ts = now_ms();
                    if let Some(timeline) = bus.read_mut::<Timeline>() {
                        timeline.push(TimelineEvent::NodeEnter {
                            node_id: fanin_id.clone(),
                            node_label: "FanIn".to_string(),
                            timestamp: fanin_enter_ts,
                        });
                        timeline.push(TimelineEvent::NodeExit {
                            node_id: fanin_id.clone(),
                            outcome_type: outcome_type_name(&combined),
                            duration_ms: 0,
                            timestamp: fanin_enter_ts,
                        });
                    }

                    // Persistence
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
                            Some(fanin_id.clone()),
                            outcome_kind_name(&combined),
                            Some(combined.to_json_value()),
                        )
                        .await;
                    }

                    combined
                })
            },
        );

        Axon {
            schematic,
            executor: next_executor,
            execution_mode,
            persistence_store,
            audit_sink,
            dlq_sink,
            dlq_policy,
            dynamic_dlq_policy,
            saga_policy,
            dynamic_saga_policy,
            saga_compensation_registry,
            iam_handle,
        }
    }
}
