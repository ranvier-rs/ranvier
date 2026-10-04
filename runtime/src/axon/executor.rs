use ranvier_core::bus::Bus;
use ranvier_core::cancellation::{CancellationContext, CancellationToken};
use ranvier_core::outcome::Outcome;
use ranvier_core::saga::{SagaPolicy, SagaStack};
use ranvier_core::telemetry::InterventionEvent;
use ranvier_core::timeline::{Timeline, TimelineEvent};
use serde::{Serialize, de::DeserializeOwned};
use std::panic::AssertUnwindSafe;
use tracing::Instrument;

use super::*;
use super::{
    ExecutionMode, ManualJump, ResumptionState, StartStep, compensation_auto_trigger,
    compensation_retry_policy, completion_from_outcome, ensure_timeline, extract_panic_message,
    load_persistence_version, maybe_export_timeline, now_ms, outcome_kind_name, outcome_target,
    outcome_type_name, persist_completion, persist_execution_event, persistence_auto_complete,
    persistence_trace_id, run_compensation, should_attach_timeline,
};

use crate::persistence::{
    CompensationContext, CompensationHandle, CompensationIdempotencyHandle, CompletionState,
    PersistenceHandle,
};

/// Apply the same configured IAM boundary before ordinary and streaming effects.
pub(crate) async fn check_iam_boundary(
    iam_handle: &Option<ranvier_core::iam::IamHandle>,
    bus: &mut Bus,
) -> Option<&'static str> {
    if let Some(iam) = iam_handle {
        use ranvier_core::iam::{IamIdentity, IamPolicy, IamToken, enforce_policy};

        if matches!(iam.policy, IamPolicy::None) {
            // No verification required — skip
        } else {
            // Required verification cannot reuse a previous request's identity.
            bus.remove::<IamIdentity>();
            if bus.has::<IamIdentity>() {
                return Some("iam.context_clear_failed");
            }
            let token = bus.read::<IamToken>().map(|t| t.0.clone());

            match token {
                Some(raw_token) => {
                    use futures_util::FutureExt as _;
                    let verification =
                        AssertUnwindSafe(async { iam.verifier.verify(&raw_token).await })
                            .catch_unwind()
                            .await;
                    match verification {
                        Ok(Ok(identity)) => {
                            if enforce_policy(&iam.policy, &identity).is_err() {
                                tracing::warn!("IAM policy enforcement failed");
                                return Some("iam.policy_denied");
                            }
                            // Insert verified identity into Bus for downstream access
                            if bus.try_insert_shared(identity).is_err() {
                                return Some("iam.context_write_denied");
                            }
                        }
                        Ok(Err(_)) => {
                            tracing::warn!("IAM token verification failed");
                            return Some("iam.verification_failed");
                        }
                        Err(_) => {
                            tracing::warn!("IAM verifier panicked");
                            return Some("iam.verifier_panicked");
                        }
                    }
                }
                None => {
                    tracing::warn!("IAM policy requires token but none found in Bus");
                    return Some("iam.missing_token");
                }
            }
        }
    }

    None
}

impl<In, Out, E, Res> Axon<In, Out, E, Res>
where
    In: Send + Sync + Serialize + DeserializeOwned + 'static,
    Out: Send + Sync + Serialize + DeserializeOwned + 'static,
    E: Send + Sync + Serialize + DeserializeOwned + std::fmt::Debug + 'static,
    Res: Send + Sync + 'static,
{
    /// Private explicit owned child entry. The active root store remains the owner;
    /// this scope supplies a distinct typed resource allocation without cloning it.
    pub(crate) async fn execute_owned_child(
        &self,
        input: In,
        resources: std::sync::Arc<Res>,
        bus: &mut Bus,
    ) -> crate::ExecutionReport<Outcome<Out, E>> {
        let mut scope = crate::nested_custody::OwnedResourceScope::begin(bus, resources.clone());
        if !scope.eligible {
            return crate::execution_observation::InvocationOwner::begin(scope.bus()).finish(
                Outcome::emit("execution.owned_child_requires_supervisor", None),
            );
        }
        self.execute(input, resources.as_ref(), scope.bus()).await
    }
    pub(crate) async fn execute_owned_child_cancellable(
        &self,
        input: In,
        resources: std::sync::Arc<Res>,
        bus: &mut Bus,
        token: CancellationToken,
    ) -> crate::ExecutionReport<ExecutionTerminal<Out, E>> {
        let mut scope = crate::nested_custody::OwnedResourceScope::begin(bus, resources.clone());
        if !scope.eligible {
            return crate::execution_observation::InvocationOwner::begin(scope.bus()).finish(
                ExecutionTerminal::Outcome(Outcome::emit(
                    "execution.owned_child_requires_supervisor",
                    None,
                )),
            );
        }
        self.execute_cancellable(input, resources.as_ref(), scope.bus(), token)
            .await
    }
    fn prepare_owned_child(
        &self, resources: &Res, bus: &Bus,
    ) -> Result<Option<(Self, crate::nested_custody::WeakCustody)>, &'static str> {
        use crate::nested_custody::{NestedSagaCustody, OwnedResources, RequiredOwnedCustody};
        if !bus.has::<RequiredOwnedCustody>() || !bus.has::<crate::execution_observation::ActiveObservation>() {
            return Ok(None);
        }
        let owned = bus.read::<OwnedResources<Res>>()
            .filter(|owned| std::ptr::eq(owned.0.as_ref(), resources))
            .ok_or("execution.nested_resources_unowned")?;
        let handlers = self.saga_compensation_registry.try_read()
            .map_err(|_| "execution.nested_registry_unavailable")?.handlers.clone();
        let mut graph = self.clone();
        graph.saga_compensation_registry = std::sync::Arc::new(std::sync::RwLock::new(
            ranvier_core::saga::SagaCompensationRegistry { handlers: handlers.clone() }));
        let custody = std::sync::Arc::new(NestedSagaCustody {
            resources: owned.0.clone(),
            handlers: ranvier_core::saga::SagaCompensationRegistry { handlers },
        });
        let retained = if let Some(store) = bus.read::<crate::nested_custody::CustodyStore>() {
            store.reserve(custody)?
        } else if let Some(client) = bus.read::<crate::nested_custody::WeakCustodyClient>() {
            client.reserve(custody)?
        } else { return Err("execution.nested_custody_unavailable"); };
        Ok(Some((graph, retained)))
    }
    fn record_owned_child(bus: &mut Bus, custody: crate::nested_custody::WeakCustody) {
        let journal = crate::SagaRollbackJournal::new(&persistence_trace_id(bus));
        journal.retain_custody(custody);
        journal.finish();
        bus.insert(journal);
    }

    /// Execute with an explicit cooperative cancellation owner.
    ///
    /// Both execution paths return domain/infrastructure and durability reports. When
    /// cancellation wins, the in-flight executor future is dropped and this
    /// outer owner performs persistence and compensation cleanup before
    /// returning the structured terminal.
    pub async fn execute_cancellable(
        &self,
        input: In,
        resources: &Res,
        bus: &mut Bus,
        token: CancellationToken,
    ) -> crate::ExecutionReport<ExecutionTerminal<Out, E>> {
        let mut owner = crate::execution_observation::InvocationOwner::begin(bus);
        let prepared = self.prepare_owned_child(resources, owner.bus());
        let selected;
        let graph = match prepared {
            Ok(Some((graph, custody))) => {
                Self::record_owned_child(owner.bus(), custody);
                selected = graph;
                &selected
            }
            Ok(None) => self,
            Err(signal) => return owner.finish(ExecutionTerminal::Outcome(Outcome::emit(signal, None))),
        };
        let terminal = graph.execute_terminal_domain(input, resources, owner.bus(), token).await;
        owner.finish(terminal)
    }

    /// Shared cancellation owner for ordinary execution and typed stream handoff.
    async fn execute_terminal_domain(
        &self,
        input: In,
        resources: &Res,
        bus: &mut Bus,
        token: CancellationToken,
    ) -> ExecutionTerminal<Out, E> {
        bus.set_cancellation_token(token.clone());
        // Session owns InvocationTrace for both execution and cancellation
        // cleanup. Do not promote its generated identity to caller-owned
        // PersistenceTraceId: that would leak into later uses of this Bus.
        let caller_owned_timeline = bus.read::<Timeline>().is_some();

        let race = {
            let execution = self.execute_domain(input, resources, bus);
            tokio::pin!(execution);
            tokio::select! {
                biased;
                context = token.cancelled() => Err(context),
                outcome = &mut execution => Ok(outcome),
            }
        };

        match race {
            Err(context) => {
                // Dropping the execution future releases its mutable Bus
                // borrow. A transition may have been dropped between
                // installing and clearing its access policy, so cleanup
                // explicitly restores framework access first.
                bus.clear_access_policy();
                // Cleanup is shielded from the workflow token that just won.
                // The caller still owns that original token and terminal
                // context; compensation code receives a fresh control plane
                // until an outer managed-drain deadline aborts this task.
                bus.set_cancellation_token(CancellationToken::new());
                self.finish_cancellation(resources, bus, context.clone())
                    .await;
                if !caller_owned_timeline {
                    let _ = bus.remove::<Timeline>();
                }
                ExecutionTerminal::Cancelled(context)
            }
            Ok(outcome) => ExecutionTerminal::Outcome(outcome),
        }
    }

    /// Execute a whole configured prefix and its initializer under one context owner.
    /// The builder/consumer cutover wires this typed boundary instead of the raw executor.
    #[cfg(feature = "streaming")]
    pub(crate) async fn initialize_stream<Item, SErr, S>(
        &self,
        input: In,
        resources: &Res,
        bus: &mut Bus,
        streaming: &S,
        cancellation: Option<CancellationToken>,
    ) -> crate::streaming_initialization::StreamInitializationReport<
        Out,
        std::pin::Pin<Box<dyn futures_core::Stream<Item = Item> + Send>>,
        E,
    >
    where
        Item: Send + 'static,
        SErr: Send + Sync + std::fmt::Debug + 'static,
        S: ranvier_core::streaming::StreamingTransition<
                Out,
                Item = Item,
                Error = SErr,
                Resources = Res,
            >,
    {
        let mut owner = crate::execution_observation::InvocationOwner::begin(bus);
        self.initialize_stream_in_scope(input, resources, &mut owner, streaming, cancellation)
            .await
    }

    /// Initialize without consuming/restoring the caller's invocation owner.
    /// The supervisor must retain this owner through consumption and cleanup.
    #[cfg(feature = "streaming")]
    pub(crate) async fn initialize_stream_in_scope<Item, SErr, S>(
        &self,
        input: In,
        resources: &Res,
        owner: &mut crate::execution_observation::InvocationOwner<'_>,
        streaming: &S,
        cancellation: Option<CancellationToken>,
    ) -> crate::streaming_initialization::StreamInitializationReport<
        Out,
        std::pin::Pin<Box<dyn futures_core::Stream<Item = Item> + Send>>,
        E,
    >
    where
        Item: Send + 'static,
        SErr: Send + Sync + std::fmt::Debug + 'static,
        S: ranvier_core::streaming::StreamingTransition<
                Out,
                Item = Item,
                Error = SErr,
                Resources = Res,
            >,
    {
        self.initialize_stream_in_scope_with_cleanup_grace(
            input,
            resources,
            owner,
            streaming,
            cancellation,
            None,
        )
        .await
    }

    /// Initialize without consuming/restoring the caller's invocation owner.
    /// The supervisor must retain this owner through consumption and cleanup.
    #[cfg(feature = "streaming")]
    pub(crate) async fn initialize_stream_in_scope_with_cleanup_grace<Item, SErr, S>(
        &self,
        input: In,
        resources: &Res,
        owner: &mut crate::execution_observation::InvocationOwner<'_>,
        streaming: &S,
        cancellation: Option<CancellationToken>,
        cleanup_grace: Option<std::time::Duration>,
    ) -> crate::streaming_initialization::StreamInitializationReport<
        Out,
        std::pin::Pin<Box<dyn futures_core::Stream<Item = Item> + Send>>,
        E,
    >
    where
        Item: Send + 'static,
        SErr: Send + Sync + std::fmt::Debug + 'static,
        S: ranvier_core::streaming::StreamingTransition<
                Out,
                Item = Item,
                Error = SErr,
                Resources = Res,
            >,
    {
        use crate::EffectBoundary;
        use crate::streaming_initialization::{
            StreamInitializationError, StreamInitializationReport, admit_prefix,
        };
        use futures_util::FutureExt as _;
        if let Some(grace) = cleanup_grace {
            if let Some(active) = owner
                .bus()
                .read::<crate::execution_observation::ActiveObservation>()
            {
                active.0.configure_cleanup(grace);
            }
        }
        let terminal = match &cancellation {
            Some(token) => {
                self.execute_terminal_domain(input, resources, owner.bus(), token.clone())
                    .await
            }
            None => {
                ExecutionTerminal::Outcome(self.execute_domain(input, resources, owner.bus()).await)
            }
        };
        // Snapshot while the prefix's IAM, Saga and other context are still owned.
        // Unknown acknowledgments and non-Next results retain their original typed value.
        let (output, prefix) = match admit_prefix(owner.prefix_report(terminal)) {
            Ok(accepted) => accepted,
            Err(prefix) => return StreamInitializationReport::Rejected { prefix },
        };
        let initialization_saga_enabled = owner
            .bus()
            .read::<SagaPolicy>()
            .is_some_and(|policy| *policy == SagaPolicy::Enabled);
        if let Some(context) = cancellation.as_ref().and_then(CancellationToken::context) {
            return StreamInitializationReport::Cancelled {
                prefix,
                context,
                initialization: EffectBoundary::NotInvoked,
                initialization_cleanup: self
                    .cleanup_stream_initialization_with_grace(
                        resources,
                        owner.bus(),
                        initialization_saga_enabled,
                        cleanup_grace,
                    )
                    .await,
            };
        }
        let result = {
            // Construct the user future inside the unwind boundary: manual trait
            // implementations can panic before returning a future. Dropping this
            // owner still cannot await cleanup; panic hooks remain process policy.
            let initialization = async {
                AssertUnwindSafe(async {
                    streaming
                        .run_stream(output, resources, owner.bus())
                        .await
                        .map_err(|error| {
                            StreamInitializationError::Transition(format!("{error:?}"))
                        })
                })
                .catch_unwind()
                .await
                .unwrap_or(Err(StreamInitializationError::Panicked))
            };
            tokio::pin!(initialization);
            match &cancellation {
                Some(token) => tokio::select! {
                    biased;
                    context = token.cancelled() => Err(context),
                    result = &mut initialization => Ok(result),
                },
                None => Ok(initialization.await),
            }
        };
        match result {
            Ok(Ok(value)) => StreamInitializationReport::Ready { prefix, value },
            Ok(Err(error)) => StreamInitializationReport::Failed {
                prefix,
                error,
                initialization: EffectBoundary::MayHaveRun,
                initialization_cleanup: self
                    .cleanup_stream_initialization_with_grace(
                        resources,
                        owner.bus(),
                        initialization_saga_enabled,
                        cleanup_grace,
                    )
                    .await,
            },
            Err(context) => StreamInitializationReport::Cancelled {
                prefix,
                context,
                initialization: EffectBoundary::MayHaveRun,
                initialization_cleanup: self
                    .cleanup_stream_initialization_with_grace(
                        resources,
                        owner.bus(),
                        initialization_saga_enabled,
                        cleanup_grace,
                    )
                    .await,
            },
        }
    }

    /// A separate cooperative compensation budget; never reuse a cancelled workflow token.
    #[cfg(feature = "streaming")]
    pub(crate) async fn cleanup_stream_initialization_with_grace(
        &self,
        resources: &Res,
        bus: &mut Bus,
        enabled: bool,
        grace: Option<std::time::Duration>,
    ) -> Option<crate::SagaRollbackReport> {
        if !enabled {
            return None;
        }
        bus.clear_access_policy();
        bus.set_cancellation_token(CancellationToken::new());
        let trace = persistence_trace_id(bus);
        if let Some(grace) = grace {
            if let Some(active) = bus.read::<crate::execution_observation::ActiveObservation>() {
                active.0.configure_cleanup(grace);
            }
        }
        self.rollback_saga(resources, bus, &trace).await;
        // A panicked compensator may leave its access policy installed.
        bus.clear_access_policy();
        bus.read::<crate::SagaRollbackJournal>()
            .map(crate::SagaRollbackJournal::snapshot)
            .or_else(|| bus.read::<crate::SagaRollbackReport>().cloned())
    }

    /// Execute the Axon with the given input and resources.
    pub async fn execute(
        &self,
        input: In,
        resources: &Res,
        bus: &mut Bus,
    ) -> crate::ExecutionReport<Outcome<Out, E>> {
        let mut owner = crate::execution_observation::InvocationOwner::begin(bus);
        let prepared = self.prepare_owned_child(resources, owner.bus());
        let selected;
        let graph = match prepared {
            Ok(Some((graph, custody))) => {
                Self::record_owned_child(owner.bus(), custody);
                selected = graph;
                &selected
            }
            Ok(None) => self,
            Err(signal) => return owner.finish(Outcome::emit(signal, None)),
        };
        let outcome = graph.execute_domain(input, resources, owner.bus()).await;
        owner.finish(outcome)
    }

    async fn execute_domain(&self, input: In, resources: &Res, bus: &mut Bus) -> Outcome<Out, E> {
        if let ExecutionMode::Singleton {
            lock_key,
            ttl_ms,
            lock_provider,
        } = &self.execution_mode
        {
            let trace_span = tracing::info_span!("Singleton Execution", key = %lock_key);
            let _enter = trace_span.enter();
            match lock_provider.try_acquire(lock_key, *ttl_ms).await {
                Ok(true) => {
                    tracing::debug!("Successfully acquired singleton lock: {}", lock_key);
                }
                Ok(false) => {
                    tracing::debug!(
                        "Singleton lock {} already held, aborting execution.",
                        lock_key
                    );
                    // Emit a specific event indicating skip
                    return Outcome::emit(
                        "execution.skipped.lock_held",
                        Some(serde_json::json!({
                            "lock_key": lock_key
                        })),
                    );
                }
                Err(e) => {
                    tracing::error!("Failed to check singleton lock {}: {:?}", lock_key, e);
                    return Outcome::emit(
                        "execution.skipped.lock_error",
                        Some(serde_json::json!({
                            "error": e.to_string()
                        })),
                    );
                }
            }
        }

        if let Some(event) = check_iam_boundary(&self.iam_handle, bus).await {
            return Outcome::emit(event, None);
        }

        let trace_id = persistence_trace_id(bus);
        let label = self.schematic.name.clone();

        // Inject DLQ config into Bus for step-level reporting
        if let Some(sink) = &self.dlq_sink {
            bus.insert(sink.clone());
        }
        // Prefer dynamic (hot-reloadable) policy if available, otherwise use static
        let effective_dlq_policy = self
            .dynamic_dlq_policy
            .as_ref()
            .map(|d| d.current())
            .unwrap_or_else(|| self.dlq_policy.clone());
        bus.insert(effective_dlq_policy);
        bus.insert(self.schematic.clone());
        let effective_saga_policy = self
            .dynamic_saga_policy
            .as_ref()
            .map(|d| d.current())
            .unwrap_or_else(|| self.saga_policy.clone());
        bus.insert(effective_saga_policy.clone());

        if effective_saga_policy == SagaPolicy::Enabled
            && bus.has::<SagaStack>()
            && bus.read_mut::<SagaStack>().is_none()
        {
            tracing::warn!("Saga context is not writable");
            return Outcome::emit("saga.context_readonly", None);
        }

        // Initialize Saga stack if enabled and not already present (e.g. from resumption)
        if effective_saga_policy == SagaPolicy::Enabled && bus.read::<SagaStack>().is_none() {
            bus.insert(SagaStack::new());
        }

        let persistence_handle = bus.read::<PersistenceHandle>().cloned();
        let compensation_handle = bus.read::<CompensationHandle>().cloned();
        let compensation_retry_policy = compensation_retry_policy(bus);
        let compensation_idempotency = bus.read::<CompensationIdempotencyHandle>().cloned();
        let version = self.schematic.schema_version.clone();
        let migration_registry = bus
            .read::<ranvier_core::schematic::MigrationRegistry>()
            .cloned();

        let persistence_start_step = if let Some(handle) = persistence_handle.as_ref() {
            let (
                mut start_step,
                trace_version,
                intervention,
                last_node_id,
                mut last_payload,
                completion,
            ) = match load_persistence_version(handle, &trace_id).await {
                Ok(cursor) => cursor,
                Err(error) => {
                    if let Some(active) =
                        bus.read::<crate::execution_observation::ActiveObservation>()
                    {
                        active.0.block(if error.is::<RecoveryPayloadUnavailable>() {
                            crate::PersistenceOperation::RecoveryPayload
                        } else {
                            crate::PersistenceOperation::Load
                        });
                    }
                    // Internal control signal is discarded by the blocking report.
                    return Outcome::emit("execution.persistence.load_unconfirmed", None);
                }
            };
            let has_persisted_trace = trace_version.is_some();

            // Migration and interventions do not create a new execution identity.
            // An authoritative terminal trace cannot authorize further effects.
            if completion.is_some() {
                return Outcome::emit("execution.resumption.completed_trace", None);
            }

            if let Some(interv) = intervention {
                tracing::info!(
                    trace_id = %trace_id,
                    target_node = %interv.target_node,
                    "Applying manual intervention command"
                );

                // Find the step index for the target node
                if let Some(target_idx) = self
                    .schematic
                    .nodes
                    .iter()
                    .position(|n| n.id == interv.target_node || n.label == interv.target_node)
                {
                    tracing::info!(
                        trace_id = %trace_id,
                        target_node = %interv.target_node,
                        target_step = target_idx,
                        "Intervention: Jumping to target node"
                    );
                    start_step = target_idx as u64;

                    // Inject ManualJump into the bus so executors can handle skipping/payload overrides
                    bus.insert(ManualJump {
                        target_node: interv.target_node.clone(),
                        payload_override: interv.payload_override.clone(),
                    });

                    // Log audit event for intervention application
                    if let Some(sink) = self.audit_sink.as_ref() {
                        let event = InterventionEvent::ApplyIntervention {
                            workflow_id: trace_id.clone(),
                            node_id: interv.target_node.clone(),
                            timestamp: chrono::Utc::now(),
                            operator: "System".to_string(),
                            reason: Some(format!("target_step={target_idx}")),
                        };

                        let _ = sink.log_intervention(event).await;
                    }
                } else {
                    tracing::warn!(
                        trace_id = %trace_id,
                        target_node = %interv.target_node,
                        "Intervention target node not found in schematic; ignoring jump"
                    );
                }
            }

            if let Some(old_version) = trace_version
                && old_version != version
            {
                tracing::info!(
                    trace_id = %trace_id,
                    old_version = %old_version,
                    current_version = %version,
                    "Version mismatch detected during resumption"
                );

                // Try multi-hop migration path first, fall back to direct lookup
                let migration_path = migration_registry
                    .as_ref()
                    .and_then(|r| r.find_migration_path(&old_version, &version));

                let (final_migration, mapped_payload) = if let Some(path) = migration_path {
                    if path.is_empty() {
                        (None, last_payload.clone())
                    } else {
                        // Apply payload mappers along the migration chain
                        let mut payload = last_payload.clone();
                        for hop in &path {
                            if let (Some(mapper), Some(p)) = (&hop.payload_mapper, payload.as_ref())
                            {
                                match mapper.map_state(p) {
                                    Ok(mapped) => payload = Some(mapped),
                                    Err(e) => {
                                        tracing::error!(
                                            trace_id = %trace_id,
                                            from = %hop.from_version,
                                            to = %hop.to_version,
                                            error = %e,
                                            "Payload migration mapper failed"
                                        );
                                        return Outcome::emit(
                                            "execution.resumption.payload_migration_failed",
                                            Some(serde_json::json!({
                                                "trace_id": trace_id,
                                                "from": hop.from_version,
                                                "to": hop.to_version,
                                                "error": e.to_string()
                                            })),
                                        );
                                    }
                                }
                            }
                        }
                        let hops: Vec<String> = path
                            .iter()
                            .map(|h| format!("{}->{}", h.from_version, h.to_version))
                            .collect();
                        tracing::info!(trace_id = %trace_id, hops = ?hops, "Applied multi-hop migration path");
                        (path.last().copied(), payload)
                    }
                } else {
                    (None, last_payload.clone())
                };

                // Use the final migration in the path to determine strategy
                let migration = final_migration.or_else(|| {
                    migration_registry
                        .as_ref()
                        .and_then(|r| r.find_migration(&old_version, &version))
                });

                // Update last_payload with mapped version
                if mapped_payload.is_some() {
                    last_payload = mapped_payload;
                }

                let strategy = if let (Some(m), Some(node_id)) = (migration, last_node_id.as_ref())
                {
                    m.node_mapping
                        .get(node_id)
                        .cloned()
                        .unwrap_or(m.default_strategy.clone())
                } else {
                    migration
                        .map(|m| m.default_strategy.clone())
                        .unwrap_or(ranvier_core::schematic::MigrationStrategy::Fail)
                };

                match strategy {
                    ranvier_core::schematic::MigrationStrategy::ResumeFromStart => {
                        tracing::info!(trace_id = %trace_id, "Applying ResumeFromStart migration strategy");
                        start_step = 0;
                    }
                    ranvier_core::schematic::MigrationStrategy::MigrateActiveNode {
                        new_node_id,
                        ..
                    } => {
                        tracing::info!(trace_id = %trace_id, to_node = %new_node_id, "Applying MigrateActiveNode strategy");
                        if let Some(target_idx) = self
                            .schematic
                            .nodes
                            .iter()
                            .position(|n| n.id == new_node_id || n.label == new_node_id)
                        {
                            start_step = target_idx as u64;
                        } else {
                            tracing::warn!(trace_id = %trace_id, "MigrateActiveNode: target node {} not found", new_node_id);
                            return Outcome::emit(
                                "execution.resumption.migration_target_not_found",
                                Some(serde_json::json!({ "node_id": new_node_id })),
                            );
                        }
                    }
                    ranvier_core::schematic::MigrationStrategy::FallbackToNode(node_id) => {
                        tracing::info!(trace_id = %trace_id, to_node = %node_id, "Applying FallbackToNode strategy");
                        if let Some(target_idx) = self
                            .schematic
                            .nodes
                            .iter()
                            .position(|n| n.id == node_id || n.label == node_id)
                        {
                            start_step = target_idx as u64;
                        } else {
                            tracing::warn!(trace_id = %trace_id, "FallbackToNode: node {} not found", node_id);
                            return Outcome::emit(
                                "execution.resumption.migration_target_not_found",
                                Some(serde_json::json!({ "node_id": node_id })),
                            );
                        }
                    }
                    ranvier_core::schematic::MigrationStrategy::Fail => {
                        tracing::error!(trace_id = %trace_id, "Version mismatch: no migration path found. Failing resumption.");
                        return Outcome::emit(
                            "execution.resumption.version_mismatch_failed",
                            Some(serde_json::json!({
                                "trace_id": trace_id,
                                "old_version": old_version,
                                "current_version": version
                            })),
                        );
                    }
                    _ => {
                        tracing::error!(trace_id = %trace_id, "Unsupported migration strategy: {:?}", strategy);
                        return Outcome::emit(
                            "execution.resumption.unsupported_migration",
                            Some(serde_json::json!({
                                "trace_id": trace_id,
                                "strategy": format!("{:?}", strategy)
                            })),
                        );
                    }
                }
            }

            // A cursor beyond the current graph has no node at which to resume.
            // Falling through would replay the whole executor and may repeat
            // effects after an unconfirmed terminal append. Preserve the trace
            // for reconciliation until an explicit migration/intervention
            // selects a valid checkpoint.
            if has_persisted_trace
                && usize::try_from(start_step)
                    .map_or(true, |step| step >= self.schematic.nodes.len())
            {
                if let Some(active) = bus.read::<crate::execution_observation::ActiveObservation>()
                {
                    active.0.block(crate::PersistenceOperation::Terminal);
                }
                return Outcome::emit(
                    "execution.persistence.terminal_reconciliation_required",
                    None,
                );
            }

            let ingress_node_id = self.schematic.nodes.first().map(|n| n.id.clone());
            let confirmation = persist_execution_event(
                handle,
                &trace_id,
                &label,
                &version,
                start_step,
                ingress_node_id,
                "Enter",
                None,
            )
            .await;

            if confirmation == crate::execution_report::StoreConfirmation::Unconfirmed {
                if let Some(active) = bus.read::<crate::execution_observation::ActiveObservation>()
                {
                    active.0.block(crate::PersistenceOperation::Intent);
                }
                return Outcome::emit("execution.persistence.intent_unconfirmed", None);
            }

            bus.insert(StartStep(start_step));
            if start_step > 0 {
                bus.insert(ResumptionState {
                    payload: last_payload,
                });
            }

            Some(start_step)
        } else {
            None
        };

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

        let circuit_span = tracing::info_span!(
            "Circuit",
            ranvier.circuit = %label,
            ranvier.outcome_kind = tracing::field::Empty,
            ranvier.outcome_target = tracing::field::Empty
        );
        let outcome = {
            use futures_util::FutureExt as _;
            // Collected streams and raw executor bodies need not traverse
            // run_this_step. Conservatively mark the graph invocation itself
            // after persistence entry gates, before user executor code runs.
            if let Some(active) = bus.read::<crate::execution_observation::ActiveObservation>() {
                active.0.effect();
            }
            let fut = (self.executor)(input, resources, bus).instrument(circuit_span.clone());
            match AssertUnwindSafe(fut).catch_unwind().await {
                Ok(outcome) => outcome,
                Err(panic_payload) => {
                    let msg = extract_panic_message(&panic_payload);
                    tracing::error!(
                        ranvier.circuit = %label,
                        panic_message = %msg,
                        "Transition panicked during Axon execution"
                    );
                    // Try to construct Fault(E) via serde deserialization
                    match serde_json::from_value::<E>(serde_json::Value::String(format!(
                        "Transition panicked: {msg}"
                    ))) {
                        Ok(e) => Outcome::Fault(e),
                        Err(_) => {
                            // E cannot deserialize from a string; emit a panic signal instead
                            Outcome::emit(
                                "ranvier.transition.panic",
                                Some(serde_json::json!({
                                    "message": msg,
                                    "circuit": label,
                                })),
                            )
                        }
                    }
                }
            }
        };
        circuit_span.record("ranvier.outcome_kind", outcome_kind_name(&outcome));
        if let Some(target) = outcome_target(&outcome) {
            circuit_span.record("ranvier.outcome_target", tracing::field::display(&target));
        }

        // Automated Saga Rollback (LIFO)
        if matches!(outcome, Outcome::Fault(_)) && effective_saga_policy == SagaPolicy::Enabled {
            self.rollback_saga(resources, bus, &trace_id).await;
        }

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

        // A recovery failure raised inside the graph cannot certify terminal
        // history or completion. Preserve earlier history for reconciliation.
        if bus
            .read::<crate::execution_observation::ActiveObservation>()
            .is_some_and(|active| active.0.is_blocked())
        {
            return outcome;
        }

        // A transition-result write can fail after the domain effect ran. Do
        // not append a terminal row or certify completion over that unknown
        // checkpoint; preserve the domain outcome for the invocation report.
        if bus
            .read::<crate::execution_observation::ActiveObservation>()
            .is_some_and(|active| active.0.has_unconfirmed_result_write())
        {
            return outcome;
        }

        let fault_step = persistence_start_step.map(|s| s + 1).unwrap_or(1);
        let mut terminal_event_confirmed = false;
        if let Some(handle) = persistence_handle.as_ref() {
            terminal_event_confirmed = persist_encoded_execution_event(
                handle,
                &trace_id,
                &label,
                &version,
                fault_step,
                None, // Outcome-level events might not have a single node_id context here
                outcome_kind_name(&outcome),
                serde_json::to_value(&outcome).map(Some),
                bus,
            )
            .await
                == crate::execution_report::StoreConfirmation::Confirmed;
        }

        let mut completion = completion_from_outcome(&outcome);
        // External compensation reverses the observed business effect even without
        // an execution-trace store; only its trace event and completion are optional.
        if matches!(outcome, Outcome::Fault(_))
            && let Some(compensation) = compensation_handle.as_ref()
            && compensation_auto_trigger(bus)
        {
            let context = CompensationContext {
                trace_id: trace_id.clone(),
                circuit: label.clone(),
                fault_kind: outcome_kind_name(&outcome).to_string(),
                fault_step,
                timestamp_ms: now_ms(),
            };

            let journal = external_compensation_journal(bus, &trace_id);
            if run_compensation(
                compensation,
                context,
                compensation_retry_policy,
                compensation_idempotency.clone(),
                bus.read::<crate::execution_observation::ActiveObservation>()
                    .map(|active| active.0.clone()),
                Some(journal),
            )
            .await
            {
                if let Some(handle) = persistence_handle.as_ref() {
                    let compensation_event_confirmed = persist_execution_event(
                        handle,
                        &trace_id,
                        &label,
                        &version,
                        fault_step.saturating_add(1),
                        None,
                        "Compensated",
                        None,
                    )
                    .await
                        == crate::execution_report::StoreConfirmation::Confirmed;
                    terminal_event_confirmed &= compensation_event_confirmed;
                    if compensation_event_confirmed {
                        completion = CompletionState::Compensated;
                    }
                }
            }
        }

        if let Some(handle) = persistence_handle.as_ref()
            && persistence_auto_complete(bus)
            && terminal_event_confirmed
            && !persistence_encoding_failed(bus)
        {
            persist_completion(handle, &trace_id, completion).await;
        }

        if should_capture {
            maybe_export_timeline(bus, &outcome);
        }
        if inserted_timeline {
            let _ = bus.remove::<Timeline>();
        }

        outcome
    }

    /// One invocation deadline across every Saga rollback entry, including prefix cleanup.
    async fn rollback_saga(&self, resources: &Res, bus: &mut Bus, trace_id: &str) {
        #[cfg(feature = "streaming")]
        {
            bus.clear_access_policy();
            let active = bus
                .read::<crate::execution_observation::ActiveObservation>()
                .cloned();
            if let Some(active) = active {
                if let Some(deadline) = active.0.begin_cleanup() {
                    let journal = bus
                        .read::<crate::SagaRollbackJournal>()
                        .filter(|journal| journal.matches_trace(trace_id))
                        .cloned()
                        .unwrap_or_else(|| crate::SagaRollbackJournal::new(trace_id));
                    journal.mark_unfinished();
                    bus.insert(journal);
                    bus.set_cancellation_token(CancellationToken::new());
                    if deadline <= tokio::time::Instant::now() {
                        active.0.cleanup_expired();
                    } else if tokio::time::timeout_at(
                        deadline,
                        self.rollback_saga_unbounded(resources, bus, trace_id),
                    )
                    .await
                    .is_err()
                    {
                        active.0.cleanup_expired();
                    }
                    bus.clear_access_policy();
                    return;
                }
            }
        }
        self.rollback_saga_unbounded(resources, bus, trace_id).await;
    }

    async fn rollback_saga_unbounded(&self, resources: &Res, bus: &mut Bus, trace_id: &str) {
        use crate::saga_report::{SagaRollbackJournal, SagaRollbackOutcome};
        let journal = bus
            .read::<SagaRollbackJournal>()
            .filter(|journal| journal.matches_trace(trace_id))
            .cloned()
            .unwrap_or_else(|| SagaRollbackJournal::new(trace_id));
        bus.insert(journal.clone());
        while let Some(task) = {
            let mut stack = bus.read_mut::<SagaStack>();
            stack.as_mut().and_then(|stack| stack.pop())
        } {
            tracing::info!(trace_id = %trace_id, node_id = %task.node_id, "Compensating step: {}", task.node_label);

            // Journal and active hook share one immutable original allocation.
            // Timeout/drop keeps its bytes in custody without cloning snapshots.
            let task = std::sync::Arc::new(task);
            journal.push_task_shared(task.clone());
            let handler = match self.saga_compensation_registry.read() {
                Ok(registry) => registry.get(&task.node_id),
                Err(poisoned) => {
                    tracing::warn!(
                        trace_id = %trace_id,
                        node_id = %task.node_id,
                        "Saga compensation registry lock was poisoned; recovering registry for rollback lookup"
                    );
                    poisoned.into_inner().get(&task.node_id)
                }
            };
            if let Some(handler) = handler {
                bus.insert(journal.clone());
                use futures_util::FutureExt as _;
                // Factory invocation can unwind before returning its future.
                let result =
                    AssertUnwindSafe(async { handler(&task.input_snapshot, resources, bus).await })
                        .catch_unwind()
                        .await;
                // A panicked handler bypasses its normal guard removal. Journal
                // writes and the next handler belong to the framework scope.
                bus.clear_access_policy();
                let result = match result {
                    Ok(result) => result,
                    Err(_) => {
                        journal.settle(SagaRollbackOutcome::Panicked);
                        bus.insert(journal.clone());
                        tracing::error!(trace_id = %trace_id, node_id = %task.node_id, "Saga compensation panicked");
                        continue;
                    }
                };
                let outcome = match &result {
                    Outcome::Next(()) => SagaRollbackOutcome::Completed,
                    Outcome::Fault(_) => SagaRollbackOutcome::Fault,
                    Outcome::Emit(_, _) => SagaRollbackOutcome::Emitted,
                    Outcome::Branch(_, _) => SagaRollbackOutcome::Branched,
                    Outcome::Jump(_, _) => SagaRollbackOutcome::Jumped,
                };
                journal.settle(outcome);
                bus.insert(journal.clone());
                match result {
                    Outcome::Fault(_) => {
                        tracing::error!(trace_id = %trace_id, node_id = %task.node_id, "Saga compensation returned a fault");
                    }
                    Outcome::Emit(_, _) => {
                        tracing::warn!(trace_id = %trace_id, node_id = %task.node_id, "Saga compensation emitted an incomplete outcome");
                    }
                    _ => {}
                }
            } else {
                journal.settle(SagaRollbackOutcome::MissingHandler);
                tracing::warn!(trace_id = %trace_id, node_id = %task.node_id, "No compensation handler found in registry for saga rollback");
            }
        }
        journal.finish();
        let report = journal.snapshot();
        let complete = report.is_complete();
        bus.insert(journal);
        bus.insert(report);
        tracing::info!(trace_id = %trace_id, complete, "Saga automated rollback finished");
    }

    async fn finish_cancellation(
        &self,
        resources: &Res,
        bus: &mut Bus,
        context: CancellationContext,
    ) {
        let trace_id = persistence_trace_id(bus);
        let label = self.schematic.name.clone();
        let version = self.schematic.schema_version.clone();
        let persistence_handle = bus.read::<PersistenceHandle>().cloned();
        let compensation_handle = bus.read::<CompensationHandle>().cloned();
        let compensation_retry_policy = compensation_retry_policy(bus);
        let compensation_idempotency = bus.read::<CompensationIdempotencyHandle>().cloned();
        let effective_saga_policy = self
            .dynamic_saga_policy
            .as_ref()
            .map(|policy| policy.current())
            .unwrap_or_else(|| self.saga_policy.clone());

        let mut terminal_event_confirmed = false;
        let (cancellation_step, persistence_writable) = if let Some(handle) =
            persistence_handle.as_ref()
        {
            let (next_step, writable, needs_entry) = match handle.store().load(&trace_id).await {
                Ok(Some(trace)) if has_unusable_recovery_payload(&trace) => {
                    if let Some(active) =
                        bus.read::<crate::execution_observation::ActiveObservation>()
                    {
                        active.0.block(crate::PersistenceOperation::RecoveryPayload);
                    }
                    // Unknown historical effects must be reconciled before
                    // invoking cleanup or rewriting the recovery record.
                    return;
                }
                Ok(Some(trace)) if trace.completion.is_some() => {
                    tracing::warn!(
                        trace_id = %trace_id,
                        completion = ?trace.completion,
                        "Cancellation cleanup will not overwrite an already completed trace"
                    );
                    // No new undo or external compensation may be replayed for
                    // an execution already known to be terminal.
                    return;
                }
                Ok(Some(trace)) => (
                    trace
                        .events
                        .last()
                        .map(|event| event.step.saturating_add(1))
                        .unwrap_or(0),
                    true,
                    false,
                ),
                // Preserve the persistence state-machine invariant that a
                // terminal always follows a normal execution entry, even
                // when the token was already cancelled before `execute`
                // was first polled.
                Ok(None) => (1, true, true),
                Err(_) => {
                    tracing::warn!(
                        trace_id = %trace_id,
                        store_operation = "cancellation_load",
                        "Failed to inspect persistence cursor before cancellation"
                    );
                    // The next cursor is unknown. Writing at a guessed step could
                    // collide with an existing event or create a terminal without
                    // its entry marker, so leave the persisted trace untouched.
                    (0, false, false)
                }
            };
            if writable {
                if needs_entry {
                    let ingress_node_id = self.schematic.nodes.first().map(|node| node.id.clone());
                    persist_execution_event(
                        handle,
                        &trace_id,
                        &label,
                        &version,
                        0,
                        ingress_node_id,
                        "Enter",
                        None,
                    )
                    .await;
                }
                let payload = serde_json::to_value(&context).ok();
                terminal_event_confirmed = persist_execution_event(
                    handle,
                    &trace_id,
                    &label,
                    &version,
                    next_step,
                    None,
                    "Cancelled",
                    payload,
                )
                .await
                    == crate::execution_report::StoreConfirmation::Confirmed;
            }
            (next_step, writable)
        } else {
            (0, false)
        };

        if effective_saga_policy == SagaPolicy::Enabled {
            self.rollback_saga(resources, bus, &trace_id).await;
        }

        let mut completion = CompletionState::Cancelled;
        if let Some(compensation) = compensation_handle.as_ref()
            && compensation_auto_trigger(bus)
        {
            let compensation_context = CompensationContext {
                trace_id: trace_id.clone(),
                circuit: label.clone(),
                fault_kind: "Cancelled".to_string(),
                fault_step: cancellation_step,
                timestamp_ms: now_ms(),
            };

            let journal = external_compensation_journal(bus, &trace_id);
            if run_compensation(
                compensation,
                compensation_context,
                compensation_retry_policy,
                compensation_idempotency,
                bus.read::<crate::execution_observation::ActiveObservation>()
                    .map(|active| active.0.clone()),
                Some(journal),
            )
            .await
            {
                if let Some(handle) = persistence_handle.as_ref()
                    && persistence_writable
                {
                    let compensation_event_confirmed = persist_execution_event(
                        handle,
                        &trace_id,
                        &label,
                        &version,
                        cancellation_step.saturating_add(1),
                        None,
                        "Compensated",
                        None,
                    )
                    .await
                        == crate::execution_report::StoreConfirmation::Confirmed;
                    terminal_event_confirmed &= compensation_event_confirmed;
                    if compensation_event_confirmed {
                        completion = CompletionState::Compensated;
                    }
                } else {
                    terminal_event_confirmed = false;
                }
            }
        }

        if let Some(handle) = persistence_handle.as_ref()
            && persistence_writable
        {
            if persistence_auto_complete(bus)
                && terminal_event_confirmed
                && !persistence_encoding_failed(bus)
            {
                persist_completion(handle, &trace_id, completion).await;
            }
        }
    }
}

/// Convenience methods for Axons with `Res = ()` (no external resources).
impl<In, Out, E> Axon<In, Out, E, ()>
where
    In: Send + Sync + Serialize + DeserializeOwned + 'static,
    Out: Send + Sync + Serialize + DeserializeOwned + 'static,
    E: Send + Sync + Serialize + DeserializeOwned + std::fmt::Debug + 'static,
{
    /// Execute the Axon without requiring an explicit `&()` resources argument.
    ///
    /// This is a convenience wrapper around [`execute`](Self::execute) for Axons
    /// that do not use external resources (`Res = ()`).
    ///
    /// # Example
    ///
    /// ```rust
    /// use ranvier_core::{bus::Bus, outcome::Outcome};
    /// use ranvier_runtime::{Axon, ExecutionReport};
    /// # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
    /// let axon = Axon::<u32, u32, String>::new("identity");
    /// let mut bus = Bus::new();
    /// let outcome = axon.execute_simple(7, &mut bus).await;
    /// assert!(matches!(outcome, ExecutionReport::Unpersisted { result: Outcome::Next(7), .. }));
    /// // Explicit resources give the same result, using a new request bus.
    /// let mut explicit_bus = Bus::new();
    /// let explicit = axon.execute(7, &(), &mut explicit_bus).await;
    /// assert!(matches!(explicit, ExecutionReport::Unpersisted { result: Outcome::Next(7), .. }));
    /// # });
    /// ```
    pub async fn execute_simple(
        &self,
        input: In,
        bus: &mut Bus,
    ) -> crate::ExecutionReport<Outcome<Out, E>> {
        self.execute(input, &(), bus).await
    }

    /// Execute without explicit resources using cooperative cancellation.
    pub async fn execute_simple_cancellable(
        &self,
        input: In,
        bus: &mut Bus,
        token: CancellationToken,
    ) -> crate::ExecutionReport<ExecutionTerminal<Out, E>> {
        self.execute_cancellable(input, &(), bus, token).await
    }
}

#[cfg(all(test, feature = "streaming"))]
mod scoped_stream_tests {
    use super::*;
    use futures_util::StreamExt;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Resources(AtomicUsize);
    #[derive(Clone)]
    struct Effect;
    #[async_trait::async_trait]
    impl ranvier_core::Transition<(), ()> for Effect {
        type Error = String;
        type Resources = Resources;
        async fn run(&self, _: (), resources: &Resources, _: &mut Bus) -> Outcome<(), String> {
            resources.0.fetch_add(1, Ordering::SeqCst);
            Outcome::Next(())
        }
    }
    #[derive(Clone)]
    struct Init;
    #[async_trait::async_trait]
    impl ranvier_core::streaming::StreamingTransition<()> for Init {
        type Error = String;
        type Resources = Resources;
        type Item = ();
        async fn run_stream(
            &self,
            _: (),
            _: &Resources,
            _: &mut Bus,
        ) -> Result<std::pin::Pin<Box<dyn futures_core::Stream<Item = ()> + Send>>, String>
        {
            Ok(Box::pin(futures_util::stream::empty()))
        }
    }
    #[tokio::test]
    async fn exhausted_cleanup_does_not_reuse_old_finished_journal_for_new_stack() {
        let resources = Resources(AtomicUsize::new(0));
        let graph = Axon::<(), (), String, Resources>::start("expired-again");
        let mut bus = Bus::new();
        let mut owner = crate::execution_observation::InvocationOwner::begin(&mut bus);
        let trace = persistence_trace_id(owner.bus());
        let journal = crate::SagaRollbackJournal::new(&trace);
        journal.finish();
        assert!(journal.snapshot().is_complete());
        owner.bus().insert(journal);
        let mut stack = SagaStack::new();
        stack.push(
            "new-task".into(),
            "private-label".into(),
            b"private-input".to_vec(),
        );
        owner.bus().insert(stack);
        let cleanup = graph
            .cleanup_stream_initialization_with_grace(
                &resources,
                owner.bus(),
                true,
                Some(std::time::Duration::ZERO),
            )
            .await
            .unwrap();
        assert!(!cleanup.finished);
        assert!(!cleanup.is_complete());
        assert!(cleanup.steps.is_empty());
        assert_eq!(
            owner.bus().read::<SagaStack>().unwrap().tasks[0].input_snapshot,
            b"private-input"
        );
        assert_eq!(resources.0.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn scoped_child_cleanup_uses_live_registry_without_touching_parent_inputs() {
        let resources = Resources(AtomicUsize::new(0));
        let prefix = Axon::<(), (), String, Resources>::start("scoped-child")
            .with_saga_policy(SagaPolicy::Enabled)
            .then_compensated(Effect, Effect);
        let mut bus = Bus::new();
        let mut parent = crate::execution_observation::InvocationOwner::begin(&mut bus);
        let mut parent_stack = SagaStack::new();
        parent_stack.push(
            "parent-node".into(),
            "private-parent".into(),
            b"private-parent-input".to_vec(),
        );
        parent.bus().insert(parent_stack);
        let parent_trace = persistence_trace_id(parent.bus());
        {
            let mut child = crate::execution_observation::InvocationOwner::begin(parent.bus());
            let child_trace = persistence_trace_id(child.bus());
            assert_ne!(child_trace, parent_trace);
            let report = prefix
                .initialize_stream_in_scope((), &resources, &mut child, &Init, None)
                .await;
            assert!(matches!(
                report,
                crate::StreamInitializationReport::Ready { .. }
            ));
            let stack = child.bus().read::<SagaStack>().unwrap();
            assert_eq!(stack.tasks.len(), 1);
            assert_ne!(stack.tasks[0].node_id, "parent-node");
            let cleanup = prefix
                .cleanup_stream_initialization_with_grace(&resources, child.bus(), true, None)
                .await
                .unwrap();
            assert!(cleanup.is_complete());
            assert_eq!(cleanup.steps.len(), 1);
            assert_eq!(resources.0.load(Ordering::SeqCst), 2);
            assert_eq!(persistence_trace_id(child.bus()), child_trace);
            assert!(child.bus().read::<SagaStack>().unwrap().is_empty());
        }
        assert_eq!(persistence_trace_id(parent.bus()), parent_trace);
        let stack = parent.bus().read::<SagaStack>().unwrap();
        assert_eq!(stack.tasks.len(), 1);
        assert_eq!(stack.tasks[0].node_id, "parent-node");
        assert_eq!(stack.tasks[0].input_snapshot, b"private-parent-input");
    }
    #[tokio::test]
    async fn scoped_ready_keeps_original_invocation_and_pending_tasks_until_owner_drop() {
        let resources = Resources(AtomicUsize::new(0));
        let prefix = Axon::<(), (), String, Resources>::start("scoped-ready")
            .with_saga_policy(SagaPolicy::Enabled)
            .then_compensated(Effect, Effect);
        let mut bus = Bus::new();
        {
            let mut owner = crate::execution_observation::InvocationOwner::begin(&mut bus);
            let trace = persistence_trace_id(owner.bus());
            let report = prefix
                .initialize_stream_in_scope((), &resources, &mut owner, &Init, None)
                .await;
            let crate::StreamInitializationReport::Ready { mut value, .. } = report else {
                panic!("expected Ready");
            };
            assert!(value.next().await.is_none());
            assert_eq!(persistence_trace_id(owner.bus()), trace);
            assert_eq!(owner.bus().read::<SagaPolicy>(), Some(&SagaPolicy::Enabled));
            assert_eq!(owner.bus().read::<SagaStack>().unwrap().tasks.len(), 1);
            assert!(crate::execution_observation::has_stream_recovery_authority(
                owner.bus()
            ));
            assert_eq!(resources.0.load(Ordering::SeqCst), 1);
        }
        assert!(!bus.has::<crate::execution_observation::InvocationTrace>());
        assert_eq!(bus.read::<SagaStack>().unwrap().tasks.len(), 1);
    }
}
