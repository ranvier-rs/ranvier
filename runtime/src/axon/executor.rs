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

impl<In, Out, E, Res> Axon<In, Out, E, Res>
where
    In: Send + Sync + Serialize + DeserializeOwned + 'static,
    Out: Send + Sync + Serialize + DeserializeOwned + 'static,
    E: Send + Sync + Serialize + DeserializeOwned + std::fmt::Debug + 'static,
    Res: ranvier_core::transition::ResourceRequirement,
{
    /// Execute with an explicit cooperative cancellation owner.
    ///
    /// The existing [`execute`](Self::execute) path is unchanged. When
    /// cancellation wins, the in-flight executor future is dropped and this
    /// outer owner performs persistence and compensation cleanup before
    /// returning the structured terminal.
    pub async fn execute_cancellable(
        &self,
        input: In,
        resources: &Res,
        bus: &mut Bus,
        token: CancellationToken,
    ) -> ExecutionTerminal<Out, E> {
        bus.set_cancellation_token(token.clone());
        if bus
            .read::<crate::persistence::PersistenceTraceId>()
            .is_none()
        {
            let trace_id = persistence_trace_id(bus);
            bus.insert(crate::persistence::PersistenceTraceId(trace_id));
        }
        let caller_owned_timeline = bus.read::<Timeline>().is_some();

        let race = {
            let execution = self.execute(input, resources, bus);
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

    /// Execute the Axon with the given input and resources.
    pub async fn execute(&self, input: In, resources: &Res, bus: &mut Bus) -> Outcome<Out, E> {
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

        // ── IAM Boundary Check ────────────────────────────────────
        if let Some(iam) = &self.iam_handle {
            use ranvier_core::iam::{IamPolicy, IamToken, enforce_policy};

            if matches!(iam.policy, IamPolicy::None) {
                // No verification required — skip
            } else {
                let token = bus.read::<IamToken>().map(|t| t.0.clone());

                match token {
                    Some(raw_token) => {
                        match iam.verifier.verify(&raw_token).await {
                            Ok(identity) => {
                                if let Err(e) = enforce_policy(&iam.policy, &identity) {
                                    tracing::warn!(
                                        policy = ?iam.policy,
                                        subject = %identity.subject,
                                        "IAM policy enforcement failed: {}",
                                        e
                                    );
                                    return Outcome::emit(
                                        "iam.policy_denied",
                                        Some(serde_json::json!({
                                            "error": e.to_string(),
                                            "subject": identity.subject,
                                        })),
                                    );
                                }
                                // Insert verified identity into Bus for downstream access
                                bus.insert_shared(identity);
                            }
                            Err(e) => {
                                tracing::warn!("IAM token verification failed: {}", e);
                                return Outcome::emit(
                                    "iam.verification_failed",
                                    Some(serde_json::json!({
                                        "error": e.to_string()
                                    })),
                                );
                            }
                        }
                    }
                    None => {
                        tracing::warn!("IAM policy requires token but none found in Bus");
                        return Outcome::emit("iam.missing_token", None);
                    }
                }
            }
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
            let (mut start_step, trace_version, intervention, last_node_id, mut last_payload) =
                load_persistence_version(handle, &trace_id).await;

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

            let ingress_node_id = self.schematic.nodes.first().map(|n| n.id.clone());
            persist_execution_event(
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

        if let Some(handle) = persistence_handle.as_ref() {
            let fault_step = persistence_start_step.map(|s| s + 1).unwrap_or(1);
            persist_execution_event(
                handle,
                &trace_id,
                &label,
                &version,
                fault_step,
                None, // Outcome-level events might not have a single node_id context here
                outcome_kind_name(&outcome),
                Some(outcome.to_json_value()),
            )
            .await;

            let mut completion = completion_from_outcome(&outcome);
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

                if run_compensation(
                    compensation,
                    context,
                    compensation_retry_policy,
                    compensation_idempotency.clone(),
                )
                .await
                {
                    persist_execution_event(
                        handle,
                        &trace_id,
                        &label,
                        &version,
                        fault_step.saturating_add(1),
                        None,
                        "Compensated",
                        None,
                    )
                    .await;
                    completion = CompletionState::Compensated;
                }
            }

            if persistence_auto_complete(bus) {
                persist_completion(handle, &trace_id, completion).await;
            }
        }

        if should_capture {
            maybe_export_timeline(bus, &outcome);
        }
        if inserted_timeline {
            let _ = bus.remove::<Timeline>();
        }

        outcome
    }

    async fn rollback_saga(&self, resources: &Res, bus: &mut Bus, trace_id: &str) {
        while let Some(task) = {
            let mut stack = bus.read_mut::<SagaStack>();
            stack.as_mut().and_then(|stack| stack.pop())
        } {
            tracing::info!(trace_id = %trace_id, node_id = %task.node_id, "Compensating step: {}", task.node_label);

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
                let result = handler(task.input_snapshot, resources, bus).await;
                match result {
                    Outcome::Fault(error) => {
                        tracing::error!(trace_id = %trace_id, node_id = %task.node_id, "Saga compensation FAILED: {:?}", error);
                    }
                    Outcome::Emit(event_type, payload) => {
                        tracing::warn!(
                            trace_id = %trace_id,
                            node_id = %task.node_id,
                            event_type = %event_type,
                            payload = ?payload,
                            "Saga compensation emitted a non-fatal event"
                        );
                    }
                    _ => {}
                }
            } else {
                tracing::warn!(trace_id = %trace_id, node_id = %task.node_id, "No compensation handler found in registry for saga rollback");
            }
        }
        tracing::info!(trace_id = %trace_id, "Saga automated rollback completed");
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

        let (cancellation_step, persistence_writable) = if let Some(handle) =
            persistence_handle.as_ref()
        {
            let (next_step, writable, needs_entry) = match handle.store().load(&trace_id).await {
                Ok(Some(trace)) if trace.completion.is_some() => {
                    tracing::warn!(
                        trace_id = %trace_id,
                        completion = ?trace.completion,
                        "Cancellation cleanup will not overwrite an already completed trace"
                    );
                    (
                        trace
                            .events
                            .last()
                            .map(|event| event.step.saturating_add(1))
                            .unwrap_or(0),
                        false,
                        false,
                    )
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
                Err(error) => {
                    tracing::warn!(
                        trace_id = %trace_id,
                        error = %error,
                        "Failed to inspect persistence cursor before cancellation"
                    );
                    (0, true, false)
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
                persist_execution_event(
                    handle,
                    &trace_id,
                    &label,
                    &version,
                    next_step,
                    None,
                    "Cancelled",
                    payload,
                )
                .await;
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

            if run_compensation(
                compensation,
                compensation_context,
                compensation_retry_policy,
                compensation_idempotency,
            )
            .await
            {
                if let Some(handle) = persistence_handle.as_ref()
                    && persistence_writable
                {
                    persist_execution_event(
                        handle,
                        &trace_id,
                        &label,
                        &version,
                        cancellation_step.saturating_add(1),
                        None,
                        "Compensated",
                        None,
                    )
                    .await;
                }
                completion = CompletionState::Compensated;
            }
        }

        if let Some(handle) = persistence_handle.as_ref()
            && persistence_writable
        {
            if persistence_auto_complete(bus) {
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
    /// ```rust,ignore
    /// let outcome = pipeline.execute_simple(input, &mut bus).await;
    /// // equivalent to: pipeline.execute(input, &(), &mut bus).await
    /// ```
    pub async fn execute_simple(&self, input: In, bus: &mut Bus) -> Outcome<Out, E> {
        self.execute(input, &(), bus).await
    }

    /// Execute without explicit resources using cooperative cancellation.
    pub async fn execute_simple_cancellable(
        &self,
        input: In,
        bus: &mut Bus,
        token: CancellationToken,
    ) -> ExecutionTerminal<Out, E> {
        self.execute_cancellable(input, &(), bus, token).await
    }
}
