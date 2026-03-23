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
    BusCapabilitySchema, Edge, EdgeType, Node, NodeKind, Schematic, SourceLocation,
};
use ranvier_core::timeline::{Timeline, TimelineEvent};
use ranvier_core::transition::Transition;
#[cfg(feature = "streaming")]
use ranvier_core::streaming::{StreamTimeoutConfig, StreamingTransition};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::any::type_name;
use std::ffi::OsString;
use std::fs;
use std::future::Future;
use std::panic::{AssertUnwindSafe, Location};
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
    ///         else { Outcome::Next(order) }
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

impl<In, Out, E, Res> Axon<In, Out, E, Res>
where
    In: Send + Sync + Serialize + DeserializeOwned + 'static,
    Out: Send + Sync + Serialize + DeserializeOwned + 'static,
    E: Send + Sync + Serialize + DeserializeOwned + std::fmt::Debug + 'static,
    Res: ranvier_core::transition::ResourceRequirement,
{
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
        S: AuditSink + 'static,
    {
        self.audit_sink = Some(Arc::new(sink));
        self
    }

    /// Set the Dead Letter Queue sink for this Axon.
    pub fn with_dlq_sink<S>(mut self, sink: S) -> Self
    where
        S: DlqSink + 'static,
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
}

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

impl<In, Out, E, Res> Axon<In, Out, E, Res>
where
    In: Send + Sync + Serialize + DeserializeOwned + 'static,
    Out: Send + Sync + Serialize + DeserializeOwned + 'static,
    E: Send + Sync + Serialize + DeserializeOwned + std::fmt::Debug + 'static,
    Res: ranvier_core::transition::ResourceRequirement,
{
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

                    last_result.unwrap_or_else(|| {
                        Outcome::emit("execution.retry.exhausted", None)
                    })
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
            let mut registry = saga_compensation_registry.write().expect("saga compensation registry lock poisoned");
            let comp_fn = compensation.clone();
            let transition_bus_policy = bus_policy_for_registry.clone();

            let handler: ranvier_core::saga::SagaCompensationFn<E, Res> =
                Arc::new(move |input_data, res, bus| {
                    let comp = comp_fn.clone();
                    let bus_policy = transition_bus_policy.clone();
                    Box::pin(async move {
                        let input: Out = serde_json::from_slice(&input_data).expect("saga compensation input deserialization failed — type mismatch between snapshot and compensation handler");
                        bus.set_access_policy(comp.label(), bus_policy);
                        let res = comp.run(input, res, bus).await;
                        bus.clear_access_policy();
                        res
                    })
                });
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

    /// Run multiple transitions in parallel (fan-out / fan-in).
    ///
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
        let stream_executor: crate::streaming_axon::StreamExecutorType<In, Item, E, Res> =
            Arc::new(
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
                            .map_err(|e| {
                                StreamingAxonError::StreamInitError(format!("{:?}", e))
                            })
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
                                bus.insert(identity);
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
                        let event = AuditEvent::new(
                            uuid::Uuid::new_v4().to_string(),
                            "System".to_string(),
                            "ApplyIntervention".to_string(),
                            trace_id.to_string(),
                        )
                        .with_metadata("target_node", interv.target_node.clone())
                        .with_metadata("target_step", target_idx);

                        let _ = sink.append(&event).await;
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
            let fut = (self.executor)(input, resources, bus)
                .instrument(circuit_span.clone());
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
        if matches!(outcome, Outcome::Fault(_)) && self.saga_policy == SagaPolicy::Enabled {
            while let Some(task) = {
                let mut stack = bus.read_mut::<SagaStack>();
                stack.as_mut().and_then(|s| s.pop())
            } {
                tracing::info!(trace_id = %trace_id, node_id = %task.node_id, "Compensating step: {}", task.node_label);

                let handler = {
                    let registry = self.saga_compensation_registry.read().expect("saga compensation registry lock poisoned");
                    registry.get(&task.node_id)
                };
                if let Some(handler) = handler {
                    let res = handler(task.input_snapshot, resources, bus).await;
                    if let Outcome::Fault(e) = res {
                        tracing::error!(trace_id = %trace_id, node_id = %task.node_id, "Saga compensation FAILED: {:?}", e);
                    }
                } else {
                    tracing::warn!(trace_id = %trace_id, node_id = %task.node_id, "No compensation handler found in registry for saga rollback");
                }
            }
            tracing::info!(trace_id = %trace_id, "Saga automated rollback completed");
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
