use ranvier_core::bus::Bus;
use ranvier_core::outcome::Outcome;
use ranvier_core::schematic::{Edge, EdgeType, Node, NodeKind, SourceLocation};
use ranvier_core::timeline::{Timeline, TimelineEvent};
use ranvier_core::transition::Transition;
use serde::{Serialize, de::DeserializeOwned};
use std::panic::Location;
use std::sync::Arc;
use std::time::Instant;

use crate::persistence::PersistenceHandle;

use super::*;
use super::{
    bus_capability_schema_from_policy, now_ms, outcome_kind_name, outcome_type_name,
    persist_execution_event, persistence_trace_id, type_name_of,
};

struct ParallelBranchResult<Out, E> {
    index: usize,
    node_id: String,
    label: String,
    outcome: Outcome<Out, E>,
    entered_at_ms: u64,
    exited_at_ms: u64,
    duration_ms: u64,
}

type KeyedTimelineEvent = (u64, u8, usize, TimelineEvent);

fn sort_parallel_branch_events(events: &mut [KeyedTimelineEvent]) {
    events.sort_by_key(|(timestamp, phase, index, _)| (*timestamp, *phase, *index));
}

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
    /// Each parallel branch receives its own fresh [`Bus`] instance. This
    /// preserves the 0.51.x behavior. Use
    /// [`parallel_with_bus_policy`](Self::parallel_with_bus_policy) to inherit
    /// explicitly shared request context.
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
        self.parallel_with_bus_policy(transitions, strategy, ParallelBusPolicy::Isolated)
    }

    /// Run parallel branches with an explicit Bus inheritance policy.
    ///
    /// [`ParallelBusPolicy::InheritShared`] gives each branch a local Bus
    /// overlay over values inserted into the parent with
    /// [`Bus::insert_shared`] or [`Bus::provide_shared`]. Inherited values are
    /// read-only. Branch-local writes are discarded, and no implicit merge is
    /// performed. [`ParallelBusPolicy::Isolated`] is equivalent to
    /// [`parallel`](Self::parallel).
    #[track_caller]
    pub fn parallel_with_bus_policy(
        self,
        transitions: Vec<Arc<dyn Transition<Out, Out, Resources = Res, Error = E> + Send + Sync>>,
        strategy: ParallelStrategy,
        bus_policy: ParallelBusPolicy,
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
                "Parallel split ({} branches, {:?}, bus={:?})",
                transitions.len(),
                strategy,
                bus_policy
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
            description: Some(format!(
                "Parallel join ({:?}, bus={:?})",
                strategy, bus_policy
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
                let bus_policy = bus_policy;

                Box::pin(async move {
                    // Run previous steps
                    let prev_result = prev(input, res, bus).await;
                    let state = match prev_result {
                        Outcome::Next(t) => t,
                        other => return other.map(|_| unreachable!()),
                    };

                    // Timeline: FanOut enter
                    let fanout_started = Instant::now();
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
                    // without &mut Bus aliasing. Only the explicit policy can
                    // add read-only inherited context.
                    let cancellation_token = bus.cancellation_token().cloned();
                    let futs: Vec<_> = branches
                        .iter()
                        .enumerate()
                        .map(|(i, trans)| {
                            let branch_state = state.clone();
                            let branch_node_id = branch_ids[i].clone();
                            let trans = trans.clone();
                            let mut branch_bus = match bus_policy {
                                ParallelBusPolicy::Isolated => Bus::new(),
                                ParallelBusPolicy::InheritShared => bus.fork_for_parallel(),
                            };
                            if let Some(token) = cancellation_token.clone() {
                                branch_bus.set_cancellation_token(token);
                            }

                            async move {
                                let mut branch_bus = branch_bus;
                                let label = trans.label();
                                let bus_policy = trans.bus_access_policy();

                                branch_bus.set_access_policy(label.clone(), bus_policy);
                                let entered_at_ms = now_ms();
                                let started = Instant::now();
                                let result = trans.run(branch_state, res, &mut branch_bus).await;
                                let duration_ms = started.elapsed().as_millis() as u64;
                                let exited_at_ms = now_ms().max(entered_at_ms);
                                branch_bus.clear_access_policy();

                                ParallelBranchResult {
                                    index: i,
                                    node_id: branch_node_id,
                                    label,
                                    outcome: result,
                                    entered_at_ms,
                                    exited_at_ms,
                                    duration_ms,
                                }
                            }
                        })
                        .collect();

                    // Run all branches concurrently within the current task
                    let results: Vec<ParallelBranchResult<Out, E>> =
                        futures_util::future::join_all(futs).await;

                    // Timeline: branch timestamps are captured inside each
                    // future, then emitted deterministically. Enter precedes
                    // exit and declaration index breaks equal timestamp ties.
                    if let Some(timeline) = bus.read_mut::<Timeline>() {
                        let mut branch_events = Vec::with_capacity(results.len() * 2);
                        for result in &results {
                            branch_events.push((
                                result.entered_at_ms,
                                0_u8,
                                result.index,
                                TimelineEvent::NodeEnter {
                                    node_id: result.node_id.clone(),
                                    node_label: result.label.clone(),
                                    timestamp: result.entered_at_ms,
                                },
                            ));
                            branch_events.push((
                                result.exited_at_ms,
                                1_u8,
                                result.index,
                                TimelineEvent::NodeExit {
                                    node_id: result.node_id.clone(),
                                    outcome_type: outcome_type_name(&result.outcome),
                                    duration_ms: result.duration_ms,
                                    timestamp: result.exited_at_ms,
                                },
                            ));
                        }
                        sort_parallel_branch_events(&mut branch_events);
                        for (_, _, _, event) in branch_events {
                            timeline.push(event);
                        }
                    }

                    // Timeline: FanOut exit
                    if let Some(timeline) = bus.read_mut::<Timeline>() {
                        let fanout_exit_ts = now_ms().max(fanout_enter_ts);
                        timeline.push(TimelineEvent::NodeExit {
                            node_id: fanout_id.clone(),
                            outcome_type: "Next".to_string(),
                            duration_ms: fanout_started.elapsed().as_millis() as u64,
                            timestamp: fanout_exit_ts,
                        });
                    }

                    // Timeline: FanIn starts before deterministic strategy
                    // combination and exits after the result is selected.
                    let fanin_started = Instant::now();
                    let fanin_enter_ts = now_ms();
                    if let Some(timeline) = bus.read_mut::<Timeline>() {
                        timeline.push(TimelineEvent::NodeEnter {
                            node_id: fanin_id.clone(),
                            node_label: "FanIn".to_string(),
                            timestamp: fanin_enter_ts,
                        });
                    }

                    // ── Apply strategy ─────────────────────────────
                    let combined = match strategy {
                        ParallelStrategy::AllMustSucceed => {
                            let mut first_fault = None;
                            let mut first_success = None;

                            for result in results {
                                match result.outcome {
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

                            for result in results {
                                match result.outcome {
                                    Outcome::Next(val) if first_success.is_none() => {
                                        first_success = Some(Outcome::Next(val));
                                    }
                                    Outcome::Fault(e) if first_fault.is_none() => {
                                        first_fault = Some(Outcome::Fault(e));
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

                    // Timeline: FanIn exit
                    if let Some(timeline) = bus.read_mut::<Timeline>() {
                        timeline.push(TimelineEvent::NodeExit {
                            node_id: fanin_id.clone(),
                            outcome_type: outcome_type_name(&combined),
                            duration_ms: fanin_started.elapsed().as_millis() as u64,
                            timestamp: now_ms().max(fanin_enter_ts),
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

#[cfg(test)]
mod tests {
    use super::{KeyedTimelineEvent, sort_parallel_branch_events};
    use ranvier_core::timeline::TimelineEvent;

    #[test]
    fn parallel_timeline_ties_order_enter_before_exit_then_by_declaration() {
        let timestamp = 42;
        let mut events: Vec<KeyedTimelineEvent> = vec![
            (
                timestamp,
                1,
                1,
                TimelineEvent::NodeExit {
                    node_id: "branch-1".to_string(),
                    outcome_type: "Next".to_string(),
                    duration_ms: 0,
                    timestamp,
                },
            ),
            (
                timestamp,
                0,
                1,
                TimelineEvent::NodeEnter {
                    node_id: "branch-1".to_string(),
                    node_label: "branch-1".to_string(),
                    timestamp,
                },
            ),
            (
                timestamp,
                1,
                0,
                TimelineEvent::NodeExit {
                    node_id: "branch-0".to_string(),
                    outcome_type: "Next".to_string(),
                    duration_ms: 0,
                    timestamp,
                },
            ),
            (
                timestamp,
                0,
                0,
                TimelineEvent::NodeEnter {
                    node_id: "branch-0".to_string(),
                    node_label: "branch-0".to_string(),
                    timestamp,
                },
            ),
        ];

        sort_parallel_branch_events(&mut events);

        let ordered = events
            .iter()
            .map(|(_, _, _, event)| match event {
                TimelineEvent::NodeEnter { node_id, .. } => ("enter", node_id.as_str()),
                TimelineEvent::NodeExit { node_id, .. } => ("exit", node_id.as_str()),
                _ => unreachable!("test contains only branch enter/exit events"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            ordered,
            vec![
                ("enter", "branch-0"),
                ("enter", "branch-1"),
                ("exit", "branch-0"),
                ("exit", "branch-1"),
            ]
        );
    }
}
