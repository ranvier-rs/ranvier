use ranvier_core::bus::Bus;
use ranvier_core::event::DlqPolicy;
use ranvier_core::outcome::Outcome;
use ranvier_core::policy::DynamicPolicy;
use ranvier_core::saga::SagaPolicy;
use ranvier_core::schematic::{Edge, EdgeType, Node, NodeKind, Schematic, SourceLocation};
#[cfg(feature = "streaming")]
use ranvier_core::streaming::{StreamTimeoutConfig, StreamingTransition};
use ranvier_core::timeline::{Timeline, TimelineEvent};
use ranvier_core::transition::Transition;
use serde::{Serialize, de::DeserializeOwned};
use std::fs;
use std::panic::Location;
use std::sync::Arc;

use super::*;
use super::{
    bus_capability_schema_from_policy, inspector_dev_mode_from_env, inspector_enabled_from_env,
    now_ms, run_this_compensated_step, run_this_step, schematic_export_request_from_process,
    type_name_of,
};

// ---------------------------------------------------------------------------
// Block 1: Constructors (identity Axon: In -> In)
// ---------------------------------------------------------------------------

impl<In, E, Res> Axon<In, In, E, Res>
where
    In: Send + Sync + Serialize + DeserializeOwned + 'static,
    E: Send + Sync + Serialize + DeserializeOwned + std::fmt::Debug + 'static,
    Res: ranvier_core::transition::ResourceRequirement,
{
    /// Create a new Axon flow with the given label.
    /// This is the preferred entry point per Flat API guidelines.
    #[track_caller]
    pub fn new(label: &str) -> Self {
        let caller = Location::caller();
        Self::start_with_source(label, caller)
    }

    /// Start defining a new Axon flow.
    /// This creates an Identity Axon (In -> In) with no initial resource requirements.
    #[track_caller]
    pub fn start(label: &str) -> Self {
        let caller = Location::caller();
        Self::start_with_source(label, caller)
    }

    fn start_with_source(label: &str, caller: &'static Location<'static>) -> Self {
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
            bus_capability: None,
            source_location: Some(SourceLocation::new(caller.file(), caller.line())),
            position: None,
            compensation_node_id: None,
            input_schema: None,
            output_schema: None,
            item_type: None,
            terminal: None,
        };

        let mut schematic = Schematic::new(label);
        schematic.nodes.push(node);

        let executor: Executor<In, In, E, Res> =
            Arc::new(move |input, _res, _bus| Box::pin(std::future::ready(Outcome::Next(input))));

        Self {
            schematic,
            executor,
            execution_mode: ExecutionMode::Local,
            persistence_store: None,
            audit_sink: None,
            dlq_sink: None,
            dlq_policy: DlqPolicy::default(),
            dynamic_dlq_policy: None,
            saga_policy: SagaPolicy::default(),
            dynamic_saga_policy: None,
            saga_compensation_registry: Arc::new(std::sync::RwLock::new(
                ranvier_core::saga::SagaCompensationRegistry::new(),
            )),
            iam_handle: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Block 2: Simple / Typed convenience constructors
// ---------------------------------------------------------------------------

impl Axon<(), (), (), ()> {
    /// Convenience constructor for simple pipelines with no input state or resources.
    ///
    /// Reduces the common `Axon::<(), (), E>::new("label")` turbofish to
    /// `Axon::simple::<E>("label")`, requiring only the error type parameter.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // Before: 3 type parameters, 2 of which are always ()
    /// let axon = Axon::<(), (), String>::new("pipeline");
    ///
    /// // After: only the error type
    /// let axon = Axon::simple::<String>("pipeline");
    /// ```
    #[track_caller]
    pub fn simple<E>(label: &str) -> Axon<(), (), E, ()>
    where
        E: Send + Sync + Serialize + DeserializeOwned + std::fmt::Debug + 'static,
    {
        let caller = Location::caller();
        <Axon<(), (), E, ()>>::start_with_source(label, caller)
    }

    /// Convenience constructor for pipelines with a typed input.
    ///
    /// Creates an identity Axon `In → In` with no resources, useful with
    /// `HttpIngress::post_typed::<T>()` where the Axon's input type must
    /// match the deserialized request body.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// #[derive(Deserialize, Serialize)]
    /// struct CreateOrder { product_id: u64, qty: u32 }
    ///
    /// let axon = Axon::typed::<CreateOrder, String>("create-order")
    ///     .then_fn("validate", |order: CreateOrder, _bus| {
    ///         if order.qty == 0 { Outcome::Fault("empty order".into()) }
    ///         else { Outcome::next(order) }
    ///     });
    /// ```
    #[track_caller]
    pub fn typed<In, E>(label: &str) -> Axon<In, In, E, ()>
    where
        In: Send + Sync + Serialize + DeserializeOwned + 'static,
        E: Send + Sync + Serialize + DeserializeOwned + std::fmt::Debug + 'static,
    {
        let caller = Location::caller();
        <Axon<In, In, E, ()>>::start_with_source(label, caller)
    }
}

// ---------------------------------------------------------------------------
// Block 3: Config + Chain + Inspector/Schematic methods
// ---------------------------------------------------------------------------

impl<In, Out, E, Res> Axon<In, Out, E, Res>
where
    In: Send + Sync + Serialize + DeserializeOwned + 'static,
    Out: Send + Sync + Serialize + DeserializeOwned + 'static,
    E: Send + Sync + Serialize + DeserializeOwned + std::fmt::Debug + 'static,
    Res: ranvier_core::transition::ResourceRequirement,
{
    // -----------------------------------------------------------------------
    // Config methods
    // -----------------------------------------------------------------------

    /// Update the Execution Mode for this Axon (e.g., Local vs Singleton).
    pub fn with_execution_mode(mut self, mode: ExecutionMode) -> Self {
        self.execution_mode = mode;
        self
    }

    /// Set the schematic version for this Axon.
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.schematic.schema_version = version.into();
        self
    }

    /// Attach a persistence store to enable state inspection via the Inspector.
    pub fn with_persistence_store<S>(mut self, store: S) -> Self
    where
        S: crate::persistence::PersistenceStore + 'static,
    {
        self.persistence_store = Some(Arc::new(store));
        self
    }

    /// Attach an audit sink for tamper-evident logging.
    pub fn with_audit_sink<S>(mut self, sink: S) -> Self
    where
        S: ranvier_audit::AuditSink + 'static,
    {
        self.audit_sink = Some(Arc::new(sink));
        self
    }

    /// Set the Dead Letter Queue sink for this Axon.
    pub fn with_dlq_sink<S>(mut self, sink: S) -> Self
    where
        S: ranvier_core::event::DlqSink + 'static,
    {
        self.dlq_sink = Some(Arc::new(sink));
        self
    }

    /// Set the Dead Letter Queue policy for this Axon.
    pub fn with_dlq_policy(mut self, policy: DlqPolicy) -> Self {
        self.dlq_policy = policy;
        self
    }

    /// Set the Saga compensation policy for this Axon.
    pub fn with_saga_policy(mut self, policy: SagaPolicy) -> Self {
        self.saga_policy = policy;
        self
    }

    /// Set a dynamic (hot-reloadable) DLQ policy. When set, the dynamic policy's
    /// current value is read at each execution, overriding the static `dlq_policy`.
    pub fn with_dynamic_dlq_policy(mut self, dynamic: DynamicPolicy<DlqPolicy>) -> Self {
        self.dynamic_dlq_policy = Some(dynamic);
        self
    }

    /// Set a dynamic (hot-reloadable) Saga policy. When set, the dynamic policy's
    /// current value is read at each execution, overriding the static `saga_policy`.
    pub fn with_dynamic_saga_policy(mut self, dynamic: DynamicPolicy<SagaPolicy>) -> Self {
        self.dynamic_saga_policy = Some(dynamic);
        self
    }

    /// Set an IAM policy and verifier for identity verification at the Axon boundary.
    ///
    /// When set, `execute()` will:
    /// 1. Read `IamToken` from the Bus (injected by the HTTP layer or test harness)
    /// 2. Verify the token using the provided verifier
    /// 3. Enforce the policy against the verified identity
    /// 4. Insert the resulting `IamIdentity` into the Bus for downstream Transitions
    pub fn with_iam(
        mut self,
        policy: ranvier_core::iam::IamPolicy,
        verifier: impl ranvier_core::iam::IamVerifier + 'static,
    ) -> Self {
        self.iam_handle = Some(ranvier_core::iam::IamHandle::new(
            policy,
            Arc::new(verifier),
        ));
        self
    }

    /// Attach a JSON Schema for the **last node's input type** in the schematic.
    ///
    /// Requires the `schema` feature and `T: schemars::JsonSchema`.
    ///
    /// ```rust,ignore
    /// let axon = Axon::new("My Circuit")
    ///     .then(ProcessStep)
    ///     .with_input_schema::<CreateUserRequest>()
    ///     .with_output_schema::<UserResponse>();
    /// ```
    #[cfg(feature = "schema")]
    pub fn with_input_schema<T>(mut self) -> Self
    where
        T: schemars::JsonSchema,
    {
        if let Some(last_node) = self.schematic.nodes.last_mut() {
            let schema = schemars::schema_for!(T);
            last_node.input_schema =
                Some(serde_json::to_value(schema).unwrap_or(serde_json::Value::Null));
        }
        self
    }

    /// Attach a JSON Schema for the **last node's output type** in the schematic.
    ///
    /// Requires the `schema` feature and `T: schemars::JsonSchema`.
    #[cfg(feature = "schema")]
    pub fn with_output_schema<T>(mut self) -> Self
    where
        T: schemars::JsonSchema,
    {
        if let Some(last_node) = self.schematic.nodes.last_mut() {
            let schema = schemars::schema_for!(T);
            last_node.output_schema =
                Some(serde_json::to_value(schema).unwrap_or(serde_json::Value::Null));
        }
        self
    }

    /// Attach a raw JSON Schema value for the **last node's input type** in the schematic.
    ///
    /// Use this for pre-built schemas without requiring the `schema` feature.
    pub fn with_input_schema_value(mut self, schema: serde_json::Value) -> Self {
        if let Some(last_node) = self.schematic.nodes.last_mut() {
            last_node.input_schema = Some(schema);
        }
        self
    }

    /// Attach a raw JSON Schema value for the **last node's output type** in the schematic.
    pub fn with_output_schema_value(mut self, schema: serde_json::Value) -> Self {
        if let Some(last_node) = self.schematic.nodes.last_mut() {
            last_node.output_schema = Some(schema);
        }
        self
    }

    // -----------------------------------------------------------------------
    // Chain methods
    // -----------------------------------------------------------------------

    /// Chain a transition to this Axon.
    ///
    /// Requires the transition to use the SAME resource bundle as the previous steps.
    #[track_caller]
    pub fn then<Next, Trans>(self, transition: Trans) -> Axon<In, Next, E, Res>
    where
        Next: Send + Sync + Serialize + DeserializeOwned + 'static,
        Trans: Transition<Out, Next, Resources = Res, Error = E> + Clone + Send + Sync + 'static,
    {
        let caller = Location::caller();
        // Decompose self to avoid partial move issues
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
            bus_capability: bus_capability_schema_from_policy(transition.bus_access_policy()),
            source_location: Some(SourceLocation::new(caller.file(), caller.line())),
            position: transition
                .position()
                .map(|(x, y)| ranvier_core::schematic::Position { x, y }),
            compensation_node_id: None,
            input_schema: transition.input_schema(),
            output_schema: None,
            item_type: None,
            terminal: None,
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
        let bus_policy_for_exec = transition.bus_access_policy();
        let bus_policy_clone = bus_policy_for_exec.clone();
        let current_step_idx = schematic.nodes.len() as u64 - 1;
        let next_executor: Executor<In, Next, E, Res> = Arc::new(
            move |input: In, res: &Res, bus: &mut Bus| -> BoxFuture<'_, Outcome<Next, E>> {
                let prev = prev_executor.clone();
                let trans = transition.clone();
                let timeline_node_id = node_id_for_exec.clone();
                let timeline_node_label = node_label_for_exec.clone();
                let transition_bus_policy = bus_policy_clone.clone();
                let step_idx = current_step_idx;

                Box::pin(async move {
                    // Check for manual intervention jump
                    if let Some(jump) = bus.read::<ManualJump>()
                        && (jump.target_node == timeline_node_id
                            || jump.target_node == timeline_node_label)
                    {
                        tracing::info!(
                            node_id = %timeline_node_id,
                            node_label = %timeline_node_label,
                            "Manual jump target reached; skipping previous steps"
                        );

                        let state = if let Some(ow) = jump.payload_override.clone() {
                            match serde_json::from_value::<Out>(ow) {
                                Ok(s) => s,
                                Err(e) => {
                                    tracing::error!(
                                        "Payload override deserialization failed: {}",
                                        e
                                    );
                                    return Outcome::emit(
                                        "execution.jump.payload_error",
                                        Some(serde_json::json!({"error": e.to_string()})),
                                    )
                                    .map(|_: ()| unreachable!());
                                }
                            }
                        } else {
                            // Default back to the provided input if this is an identity jump or types match
                            // For now, treat missing payload on a mid-flow jump as an avoidable error if Possible.
                            // In a better implementation, we'd try to load the last persisted Out for the previous step.
                            return Outcome::emit(
                                "execution.jump.missing_payload",
                                Some(serde_json::json!({"node_id": timeline_node_id})),
                            );
                        };

                        // Skip prev() and continue with trans.run(state, ...)
                        return run_this_step::<Out, Next, E, Res>(
                            &trans,
                            state,
                            res,
                            bus,
                            &timeline_node_id,
                            &timeline_node_label,
                            &transition_bus_policy,
                            step_idx,
                        )
                        .await;
                    }

                    // Handle resumption skip
                    if let Some(start) = bus.read::<StartStep>()
                        && step_idx == start.0
                        && bus.read::<ResumptionState>().is_some()
                    {
                        // Prefer fresh input (In → Out via JSON round-trip).
                        // The caller provides updated state (e.g., corrected data after a fault).
                        // Falls back to persisted checkpoint state when types are incompatible.
                        let fresh_state = serde_json::to_value(&input)
                            .ok()
                            .and_then(|v| serde_json::from_value::<Out>(v).ok());
                        let persisted_state = bus
                            .read::<ResumptionState>()
                            .and_then(|r| r.payload.clone())
                            .and_then(|p| serde_json::from_value::<Out>(p).ok());

                        if let Some(s) = fresh_state.or(persisted_state) {
                            tracing::info!(node_id = %timeline_node_id, "Resuming at checkpoint");
                            return run_this_step::<Out, Next, E, Res>(
                                &trans,
                                s,
                                res,
                                bus,
                                &timeline_node_id,
                                &timeline_node_label,
                                &transition_bus_policy,
                                step_idx,
                            )
                            .await;
                        }

                        return Outcome::emit(
                            "execution.resumption.payload_error",
                            Some(serde_json::json!({"error": "no compatible resumption state"})),
                        )
                        .map(|_: ()| unreachable!());
                    }

                    // Run previous step
                    let prev_result = prev(input, res, bus).await;

                    // Unpack
                    let state = match prev_result {
                        Outcome::Next(t) => t,
                        other => return other.map(|_| unreachable!()),
                    };

                    run_this_step::<Out, Next, E, Res>(
                        &trans,
                        state,
                        res,
                        bus,
                        &timeline_node_id,
                        &timeline_node_label,
                        &transition_bus_policy,
                        step_idx,
                    )
                    .await
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

    /// Chain a closure as a lightweight Transition step.
    ///
    /// The closure receives `(input, &mut Bus)` and returns `Outcome<Next, E>`.
    /// This is ideal for simple data transformations or validation checks that
    /// don't need a full `#[transition]` struct.
    ///
    /// The closure does not receive typed resources. For resource-dependent
    /// logic, use a full `Transition` struct with `then()`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use ranvier_runtime::Axon;
    /// use ranvier_core::prelude::*;
    ///
    /// let axon = Axon::simple::<String>("pipeline")
    ///     .then_fn("score_check", |_input: (), bus: &mut Bus| {
    ///         let score = bus.read::<u32>().copied().unwrap_or(0);
    ///         if score > 75 {
    ///             Outcome::next("REJECTED".to_string())
    ///         } else {
    ///             Outcome::next("APPROVED".to_string())
    ///         }
    ///     });
    /// ```
    #[track_caller]
    pub fn then_fn<Next, F>(self, label: &str, f: F) -> Axon<In, Next, E, Res>
    where
        Next: Send + Sync + Serialize + DeserializeOwned + 'static,
        F: Fn(Out, &mut Bus) -> Outcome<Next, E> + Clone + Send + Sync + 'static,
    {
        self.then(crate::closure_transition::ClosureTransition::new(label, f))
    }

    /// Chain a transition with a retry policy.
    ///
    /// If the transition returns `Outcome::Fault`, it will be retried up to
    /// `policy.max_retries` times with the configured backoff strategy.
    /// Timeline events are recorded for each retry attempt.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use ranvier_runtime::{Axon, RetryPolicy};
    /// use std::time::Duration;
    ///
    /// let axon = Axon::new("pipeline")
    ///     .then_with_retry(my_transition, RetryPolicy::fixed(3, Duration::from_millis(100)));
    /// ```
    #[track_caller]
    pub fn then_with_retry<Next, Trans>(
        self,
        transition: Trans,
        policy: crate::retry::RetryPolicy,
    ) -> Axon<In, Next, E, Res>
    where
        Out: Clone,
        Next: Send + Sync + Serialize + DeserializeOwned + 'static,
        Trans: Transition<Out, Next, Resources = Res, Error = E> + Clone + Send + Sync + 'static,
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
            bus_capability: bus_capability_schema_from_policy(transition.bus_access_policy()),
            source_location: Some(SourceLocation::new(caller.file(), caller.line())),
            position: transition
                .position()
                .map(|(x, y)| ranvier_core::schematic::Position { x, y }),
            compensation_node_id: None,
            input_schema: transition.input_schema(),
            output_schema: None,
            item_type: None,
            terminal: None,
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
            label: Some("Next (retryable)".to_string()),
        });

        let node_id_for_exec = next_node_id.clone();
        let node_label_for_exec = transition.label();
        let bus_policy_for_exec = transition.bus_access_policy();
        let bus_policy_clone = bus_policy_for_exec.clone();
        let current_step_idx = schematic.nodes.len() as u64 - 1;
        let next_executor: Executor<In, Next, E, Res> = Arc::new(
            move |input: In, res: &Res, bus: &mut Bus| -> BoxFuture<'_, Outcome<Next, E>> {
                let prev = prev_executor.clone();
                let trans = transition.clone();
                let timeline_node_id = node_id_for_exec.clone();
                let timeline_node_label = node_label_for_exec.clone();
                let transition_bus_policy = bus_policy_clone.clone();
                let step_idx = current_step_idx;
                let retry_policy = policy.clone();

                Box::pin(async move {
                    // Run previous step
                    let prev_result = prev(input, res, bus).await;
                    let state = match prev_result {
                        Outcome::Next(t) => t,
                        other => return other.map(|_| unreachable!()),
                    };

                    // Attempt with retries
                    let mut last_result = None;
                    for attempt in 0..=retry_policy.max_retries {
                        let attempt_state = state.clone();

                        let result = run_this_step::<Out, Next, E, Res>(
                            &trans,
                            attempt_state,
                            res,
                            bus,
                            &timeline_node_id,
                            &timeline_node_label,
                            &transition_bus_policy,
                            step_idx,
                        )
                        .await;

                        match &result {
                            Outcome::Next(_) => return result,
                            Outcome::Fault(_) if attempt < retry_policy.max_retries => {
                                let delay = retry_policy.delay_for_attempt(attempt);
                                tracing::warn!(
                                    node_id = %timeline_node_id,
                                    attempt = attempt + 1,
                                    max = retry_policy.max_retries,
                                    delay_ms = delay.as_millis() as u64,
                                    "Transition failed, retrying"
                                );
                                if let Some(timeline) = bus.read_mut::<Timeline>() {
                                    timeline.push(TimelineEvent::NodeRetry {
                                        node_id: timeline_node_id.clone(),
                                        attempt: attempt + 1,
                                        max_attempts: retry_policy.max_retries,
                                        backoff_ms: delay.as_millis() as u64,
                                        timestamp: now_ms(),
                                    });
                                }
                                tokio::time::sleep(delay).await;
                            }
                            _ => {
                                last_result = Some(result);
                                break;
                            }
                        }
                    }

                    last_result.unwrap_or_else(|| Outcome::emit("execution.retry.exhausted", None))
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

    /// Chain a transition to this Axon with a timeout guard.
    ///
    /// If the transition does not complete within the specified duration,
    /// the execution is cancelled and a `Fault` is returned using the
    /// provided error factory.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use std::time::Duration;
    ///
    /// let pipeline = Axon::simple::<String>("API")
    ///     .then_with_timeout(
    ///         slow_handler,
    ///         Duration::from_secs(5),
    ///         || "Request timed out after 5 seconds".to_string(),
    ///     );
    /// ```
    #[track_caller]
    pub fn then_with_timeout<Next, Trans, F>(
        self,
        transition: Trans,
        duration: std::time::Duration,
        make_timeout_error: F,
    ) -> Axon<In, Next, E, Res>
    where
        Next: Send + Sync + Serialize + DeserializeOwned + 'static,
        Trans: Transition<Out, Next, Resources = Res, Error = E> + Clone + Send + Sync + 'static,
        F: Fn() -> E + Clone + Send + Sync + 'static,
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
            bus_capability: bus_capability_schema_from_policy(transition.bus_access_policy()),
            source_location: Some(SourceLocation::new(caller.file(), caller.line())),
            position: transition
                .position()
                .map(|(x, y)| ranvier_core::schematic::Position { x, y }),
            compensation_node_id: None,
            input_schema: transition.input_schema(),
            output_schema: None,
            item_type: None,
            terminal: None,
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
            label: Some("Next (timeout-guarded)".to_string()),
        });

        let node_id_for_exec = next_node_id.clone();
        let node_label_for_exec = transition.label();
        let bus_policy_for_exec = transition.bus_access_policy();
        let bus_policy_clone = bus_policy_for_exec.clone();
        let current_step_idx = schematic.nodes.len() as u64 - 1;
        let next_executor: Executor<In, Next, E, Res> = Arc::new(
            move |input: In, res: &Res, bus: &mut Bus| -> BoxFuture<'_, Outcome<Next, E>> {
                let prev = prev_executor.clone();
                let trans = transition.clone();
                let timeline_node_id = node_id_for_exec.clone();
                let timeline_node_label = node_label_for_exec.clone();
                let transition_bus_policy = bus_policy_clone.clone();
                let step_idx = current_step_idx;
                let timeout_duration = duration;
                let error_factory = make_timeout_error.clone();

                Box::pin(async move {
                    // Run previous step
                    let prev_result = prev(input, res, bus).await;
                    let state = match prev_result {
                        Outcome::Next(t) => t,
                        other => return other.map(|_| unreachable!()),
                    };

                    // Execute with timeout
                    match tokio::time::timeout(
                        timeout_duration,
                        run_this_step::<Out, Next, E, Res>(
                            &trans,
                            state,
                            res,
                            bus,
                            &timeline_node_id,
                            &timeline_node_label,
                            &transition_bus_policy,
                            step_idx,
                        ),
                    )
                    .await
                    {
                        Ok(result) => result,
                        Err(_elapsed) => {
                            tracing::warn!(
                                node_id = %timeline_node_id,
                                timeout_ms = timeout_duration.as_millis() as u64,
                                "Transition timed out"
                            );
                            if let Some(timeline) = bus.read_mut::<Timeline>() {
                                timeline.push(TimelineEvent::NodeTimeout {
                                    node_id: timeline_node_id.clone(),
                                    timeout_ms: timeout_duration.as_millis() as u64,
                                    timestamp: now_ms(),
                                });
                            }
                            Outcome::Fault(error_factory())
                        }
                    }
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

    /// Chain a transition to this Axon with a Saga compensation step.
    ///
    /// If the transition fails, the compensation transition will be executed
    /// automatically if `CompensationAutoTrigger` is enabled in the Bus.
    #[track_caller]
    pub fn then_compensated<Next, Trans, Comp>(
        self,
        transition: Trans,
        compensation: Comp,
    ) -> Axon<In, Next, E, Res>
    where
        Out: Clone,
        Next: Send + Sync + Serialize + DeserializeOwned + 'static,
        Trans: Transition<Out, Next, Resources = Res, Error = E> + Clone + Send + Sync + 'static,
        Comp: Transition<Out, (), Resources = Res, Error = E> + Clone + Send + Sync + 'static,
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

        // 1. Add Primary Node
        let next_node_id = uuid::Uuid::new_v4().to_string();
        let comp_node_id = uuid::Uuid::new_v4().to_string();

        let next_node = Node {
            id: next_node_id.clone(),
            kind: NodeKind::Atom,
            label: transition.label(),
            description: transition.description(),
            input_type: type_name_of::<Out>(),
            output_type: type_name_of::<Next>(),
            resource_type: type_name_of::<Res>(),
            metadata: Default::default(),
            bus_capability: bus_capability_schema_from_policy(transition.bus_access_policy()),
            source_location: Some(SourceLocation::new(caller.file(), caller.line())),
            position: transition
                .position()
                .map(|(x, y)| ranvier_core::schematic::Position { x, y }),
            compensation_node_id: Some(comp_node_id.clone()),
            input_schema: None,
            output_schema: None,
            item_type: None,
            terminal: None,
        };

        // 2. Add Compensation Node
        let comp_node = Node {
            id: comp_node_id.clone(),
            kind: NodeKind::Atom,
            label: format!("Compensate: {}", compensation.label()),
            description: compensation.description(),
            input_type: type_name_of::<Out>(),
            output_type: "void".to_string(),
            resource_type: type_name_of::<Res>(),
            metadata: Default::default(),
            bus_capability: None,
            source_location: None,
            position: compensation
                .position()
                .map(|(x, y)| ranvier_core::schematic::Position { x, y }),
            compensation_node_id: None,
            input_schema: None,
            output_schema: None,
            item_type: None,
            terminal: None,
        };

        let last_node_id = schematic
            .nodes
            .last()
            .map(|n| n.id.clone())
            .unwrap_or_default();

        schematic.nodes.push(next_node);
        schematic.nodes.push(comp_node);
        schematic.edges.push(Edge {
            from: last_node_id,
            to: next_node_id.clone(),
            kind: EdgeType::Linear,
            label: Some("Next".to_string()),
        });

        // 3. Compose Executor with Compensation Logic
        let node_id_for_exec = next_node_id.clone();
        let comp_id_for_exec = comp_node_id.clone();
        let node_label_for_exec = transition.label();
        let bus_policy_for_exec = transition.bus_access_policy();
        let step_idx_for_exec = schematic.nodes.len() as u64 - 2;
        let comp_for_exec = compensation.clone();
        let bus_policy_for_executor = bus_policy_for_exec.clone();
        let bus_policy_for_registry = bus_policy_for_exec.clone();
        let next_executor: Executor<In, Next, E, Res> = Arc::new(
            move |input: In, res: &Res, bus: &mut Bus| -> BoxFuture<'_, Outcome<Next, E>> {
                let prev = prev_executor.clone();
                let trans = transition.clone();
                let comp = comp_for_exec.clone();
                let timeline_node_id = node_id_for_exec.clone();
                let timeline_comp_id = comp_id_for_exec.clone();
                let timeline_node_label = node_label_for_exec.clone();
                let transition_bus_policy = bus_policy_for_executor.clone();
                let step_idx = step_idx_for_exec;

                Box::pin(async move {
                    // Check for manual intervention jump
                    if let Some(jump) = bus.read::<ManualJump>()
                        && (jump.target_node == timeline_node_id
                            || jump.target_node == timeline_node_label)
                    {
                        tracing::info!(
                            node_id = %timeline_node_id,
                            node_label = %timeline_node_label,
                            "Manual jump target reached (compensated); skipping previous steps"
                        );

                        let state = if let Some(ow) = jump.payload_override.clone() {
                            match serde_json::from_value::<Out>(ow) {
                                Ok(s) => s,
                                Err(e) => {
                                    tracing::error!(
                                        "Payload override deserialization failed: {}",
                                        e
                                    );
                                    return Outcome::emit(
                                        "execution.jump.payload_error",
                                        Some(serde_json::json!({"error": e.to_string()})),
                                    )
                                    .map(|_: ()| unreachable!());
                                }
                            }
                        } else {
                            return Outcome::emit(
                                "execution.jump.missing_payload",
                                Some(serde_json::json!({"node_id": timeline_node_id})),
                            )
                            .map(|_: ()| unreachable!());
                        };

                        // Skip prev() and continue with trans.run(state, ...)
                        return run_this_compensated_step::<Out, Next, E, Res, Comp>(
                            &trans,
                            &comp,
                            state,
                            res,
                            bus,
                            &timeline_node_id,
                            &timeline_comp_id,
                            &timeline_node_label,
                            &transition_bus_policy,
                            step_idx,
                        )
                        .await;
                    }

                    // Handle resumption skip
                    if let Some(start) = bus.read::<StartStep>()
                        && step_idx == start.0
                        && bus.read::<ResumptionState>().is_some()
                    {
                        let fresh_state = serde_json::to_value(&input)
                            .ok()
                            .and_then(|v| serde_json::from_value::<Out>(v).ok());
                        let persisted_state = bus
                            .read::<ResumptionState>()
                            .and_then(|r| r.payload.clone())
                            .and_then(|p| serde_json::from_value::<Out>(p).ok());

                        if let Some(s) = fresh_state.or(persisted_state) {
                            tracing::info!(node_id = %timeline_node_id, "Resuming at checkpoint (compensated)");
                            return run_this_compensated_step::<Out, Next, E, Res, Comp>(
                                &trans,
                                &comp,
                                s,
                                res,
                                bus,
                                &timeline_node_id,
                                &timeline_comp_id,
                                &timeline_node_label,
                                &transition_bus_policy,
                                step_idx,
                            )
                            .await;
                        }

                        return Outcome::emit(
                            "execution.resumption.payload_error",
                            Some(serde_json::json!({"error": "no compatible resumption state"})),
                        )
                        .map(|_: ()| unreachable!());
                    }

                    // Run previous step
                    let prev_result = prev(input, res, bus).await;

                    // Unpack
                    let state = match prev_result {
                        Outcome::Next(t) => t,
                        other => return other.map(|_| unreachable!()),
                    };

                    run_this_compensated_step::<Out, Next, E, Res, Comp>(
                        &trans,
                        &comp,
                        state,
                        res,
                        bus,
                        &timeline_node_id,
                        &timeline_comp_id,
                        &timeline_node_label,
                        &transition_bus_policy,
                        step_idx,
                    )
                    .await
                })
            },
        );
        // 4. Register Saga Compensation if enabled
        {
            let mut registry = saga_compensation_registry
                .write()
                .expect("saga compensation registry lock poisoned");
            let comp_fn = compensation.clone();
            let transition_bus_policy = bus_policy_for_registry.clone();

            let handler: ranvier_core::saga::SagaCompensationFn<E, Res> = Arc::new(
                move |input_data, res, bus| {
                    let comp = comp_fn.clone();
                    let bus_policy = transition_bus_policy.clone();
                    Box::pin(async move {
                        let input: Out = serde_json::from_slice(&input_data).expect("saga compensation input deserialization failed — type mismatch between snapshot and compensation handler");
                        bus.set_access_policy(comp.label(), bus_policy);
                        let res = comp.run(input, res, bus).await;
                        bus.clear_access_policy();
                        res
                    })
                },
            );
            registry.register(next_node_id.clone(), handler);
        }

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

    /// Attach a compensation transition to the previously added node.
    /// This establishes a Schematic-level Saga compensation mapping.
    #[track_caller]
    pub fn compensate_with<Comp>(mut self, transition: Comp) -> Self
    where
        Comp: Transition<Out, (), Resources = Res, Error = E> + Clone + Send + Sync + 'static,
    {
        // NOTE: This currently only updates the Schematic.
        // For runtime compensation behavior, use `then_compensated`.
        let caller = Location::caller();
        let comp_node_id = uuid::Uuid::new_v4().to_string();

        let comp_node = Node {
            id: comp_node_id.clone(),
            kind: NodeKind::Atom,
            label: transition.label(),
            description: transition.description(),
            input_type: type_name_of::<Out>(),
            output_type: "void".to_string(),
            resource_type: type_name_of::<Res>(),
            metadata: Default::default(),
            bus_capability: None,
            source_location: Some(SourceLocation::new(caller.file(), caller.line())),
            position: transition
                .position()
                .map(|(x, y)| ranvier_core::schematic::Position { x, y }),
            compensation_node_id: None,
            input_schema: None,
            output_schema: None,
            item_type: None,
            terminal: None,
        };

        if let Some(last_node) = self.schematic.nodes.last_mut() {
            last_node.compensation_node_id = Some(comp_node_id.clone());
        }

        self.schematic.nodes.push(comp_node);
        self
    }

    /// Add a branch point
    #[track_caller]
    pub fn branch(mut self, branch_id: impl Into<String>, label: &str) -> Self {
        let caller = Location::caller();
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
            bus_capability: None,
            source_location: Some(SourceLocation::new(caller.file(), caller.line())),
            position: None,
            compensation_node_id: None,
            input_schema: None,
            output_schema: None,
            item_type: None,
            terminal: None,
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

    // -----------------------------------------------------------------------
    // Streaming chain methods
    // -----------------------------------------------------------------------

    /// Append a streaming transition as the **terminal** step.
    ///
    /// The returned `StreamingAxon` produces a `Stream<Item>` when executed,
    /// instead of a single `Outcome`. No further `.then()` calls are allowed.
    ///
    /// ## Example
    ///
    /// ```rust,ignore
    /// let streaming = Axon::simple::<String>("chat")
    ///     .then(ClassifyIntent)
    ///     .then_stream(SynthesizeStream);
    ///
    /// let stream = streaming.execute((), &res, &mut bus).await?;
    /// ```
    #[cfg(feature = "streaming")]
    #[track_caller]
    pub fn then_stream<Item, SErr, S>(
        self,
        streaming: S,
    ) -> crate::streaming_axon::StreamingAxon<In, Item, E, Res>
    where
        Item: Send + 'static,
        SErr: Send + Sync + std::fmt::Debug + 'static,
        S: StreamingTransition<Out, Item = Item, Error = SErr, Resources = Res>
            + Clone
            + Send
            + Sync
            + 'static,
    {
        self.then_stream_internal(streaming, None)
    }

    /// Like `then_stream`, but with a `StreamTimeoutConfig` for init/idle/total timeouts.
    #[cfg(feature = "streaming")]
    #[track_caller]
    pub fn then_stream_with_timeout<Item, SErr, S>(
        self,
        streaming: S,
        timeout_config: StreamTimeoutConfig,
    ) -> crate::streaming_axon::StreamingAxon<In, Item, E, Res>
    where
        Item: Send + 'static,
        SErr: Send + Sync + std::fmt::Debug + 'static,
        S: StreamingTransition<Out, Item = Item, Error = SErr, Resources = Res>
            + Clone
            + Send
            + Sync
            + 'static,
    {
        self.then_stream_internal(streaming, Some(timeout_config))
    }

    #[cfg(feature = "streaming")]
    fn then_stream_internal<Item, SErr, S>(
        self,
        streaming: S,
        timeout_config: Option<StreamTimeoutConfig>,
    ) -> crate::streaming_axon::StreamingAxon<In, Item, E, Res>
    where
        Item: Send + 'static,
        SErr: Send + Sync + std::fmt::Debug + 'static,
        S: StreamingTransition<Out, Item = Item, Error = SErr, Resources = Res>
            + Clone
            + Send
            + Sync
            + 'static,
    {
        use crate::streaming_axon::{StreamingAxon, StreamingAxonError};
        use ranvier_core::schematic::{Edge, EdgeType, Node, NodeKind, SourceLocation};

        let caller = std::panic::Location::caller();

        let Axon {
            mut schematic,
            executor: prev_executor,
            ..
        } = self;

        // Add streaming node to schematic
        let stream_node_id = uuid::Uuid::new_v4().to_string();
        let stream_node = Node {
            id: stream_node_id.clone(),
            kind: NodeKind::StreamingTransition,
            label: streaming.label(),
            description: streaming.description(),
            input_type: type_name_of::<Out>(),
            output_type: format!("Stream<{}>", type_name_of::<Item>()),
            resource_type: type_name_of::<Res>(),
            metadata: Default::default(),
            bus_capability: None,
            source_location: Some(SourceLocation::new(caller.file(), caller.line())),
            position: None,
            compensation_node_id: None,
            input_schema: None,
            output_schema: None,
            item_type: Some(type_name_of::<Item>()),
            terminal: Some(true),
        };

        let last_node_id = schematic
            .nodes
            .last()
            .map(|n| n.id.clone())
            .unwrap_or_default();

        schematic.nodes.push(stream_node);
        schematic.edges.push(Edge {
            from: last_node_id,
            to: stream_node_id,
            kind: EdgeType::Linear,
            label: Some("Stream".to_string()),
        });

        // Build stream executor
        let stream_executor: crate::streaming_axon::StreamExecutorType<In, Item, E, Res> = Arc::new(
            move |input: In,
                  res: &Res,
                  bus: &mut Bus|
                  -> BoxFuture<
                '_,
                Result<
                    std::pin::Pin<Box<dyn futures_core::Stream<Item = Item> + Send>>,
                    StreamingAxonError<E>,
                >,
            > {
                let prev = prev_executor.clone();
                let streaming = streaming.clone();

                Box::pin(async move {
                    // Execute prefix Axon
                    let outcome = prev(input, res, bus).await;

                    // Only Next outcome is valid for streaming
                    let intermediate = match outcome {
                        Outcome::Next(val) => val,
                        Outcome::Fault(e) => {
                            return Err(StreamingAxonError::PipelineFault(e));
                        }
                        other => {
                            return Err(StreamingAxonError::UnexpectedOutcome(format!(
                                "{:?}",
                                std::mem::discriminant(&other)
                            )));
                        }
                    };

                    // Initialize stream
                    streaming
                        .run_stream(intermediate, res, bus)
                        .await
                        .map_err(|e| StreamingAxonError::StreamInitError(format!("{:?}", e)))
                })
            },
        );

        StreamingAxon {
            schematic,
            stream_executor,
            timeout_config,
            buffer_size: 64,
        }
    }

    // -----------------------------------------------------------------------
    // Inspector / Schematic methods
    // -----------------------------------------------------------------------

    /// Starts the Ranvier Inspector for this Axon on the specified port.
    /// This spawns a background task to serve the Schematic.
    pub fn serve_inspector(self, port: u16) -> Self {
        if !inspector_dev_mode_from_env() {
            tracing::info!("Inspector disabled because RANVIER_MODE is production");
            return self;
        }
        if !inspector_enabled_from_env() {
            tracing::info!("Inspector disabled by RANVIER_INSPECTOR");
            return self;
        }

        let schematic = self.schematic.clone();
        let axon_inspector = Arc::new(self.clone());
        tokio::spawn(async move {
            if let Err(e) = ranvier_inspector::Inspector::new(schematic, port)
                .with_projection_files_from_env()
                .with_mode_from_env()
                .with_auth_policy_from_env()
                .with_bearer_token_from_env()
                .with_state_inspector(axon_inspector)
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

    /// Detect schematic export mode from runtime flags.
    ///
    /// Supported triggers:
    /// - `RANVIER_SCHEMATIC=1|true|on|yes`
    /// - `--schematic`
    ///
    /// Optional output path:
    /// - `RANVIER_SCHEMATIC_OUTPUT=<path>`
    /// - `--schematic-output <path>` / `--schematic-output=<path>`
    /// - `--output <path>` / `--output=<path>` (only relevant in schematic mode)
    pub fn schematic_export_request(&self) -> Option<SchematicExportRequest> {
        schematic_export_request_from_process()
    }

    /// Export schematic and return `true` when schematic mode is active.
    ///
    /// Use this once after circuit construction and before server/custom loops:
    ///
    /// ```rust,ignore
    /// let axon = build_axon();
    /// if axon.maybe_export_and_exit()? {
    ///     return Ok(());
    /// }
    /// // Normal runtime path...
    /// ```
    pub fn maybe_export_and_exit(&self) -> anyhow::Result<bool> {
        self.maybe_export_and_exit_with(|_| ())
    }

    /// Same as [`Self::maybe_export_and_exit`] but allows a custom hook right before export/exit.
    ///
    /// This is useful when your app has custom loop/bootstrap behavior and you want
    /// to skip or cleanup that logic in schematic mode.
    pub fn maybe_export_and_exit_with<F>(&self, on_before_exit: F) -> anyhow::Result<bool>
    where
        F: FnOnce(&SchematicExportRequest),
    {
        let Some(request) = self.schematic_export_request() else {
            return Ok(false);
        };
        on_before_exit(&request);
        self.export_schematic(&request)?;
        Ok(true)
    }

    /// Export schematic according to the provided request.
    pub fn export_schematic(&self, request: &SchematicExportRequest) -> anyhow::Result<()> {
        let json = serde_json::to_string_pretty(self.schematic())?;
        if let Some(path) = &request.output {
            if let Some(parent) = path.parent()
                && !parent.as_os_str().is_empty()
            {
                fs::create_dir_all(parent)?;
            }
            fs::write(path, json.as_bytes())?;
            return Ok(());
        }
        println!("{}", json);
        Ok(())
    }
}
