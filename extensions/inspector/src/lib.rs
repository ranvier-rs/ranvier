pub mod alert;
pub mod auth;
pub mod breakpoint;
pub mod lineage;
pub mod metrics;
pub mod payload;
pub mod prometheus;
pub mod relay;
pub mod routes;
pub mod schema;
pub mod stall;
pub mod trace_store;

use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::{
        Path as AxPath, Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    routing::get,
};
use ranvier_core::event::DlqReader;
use ranvier_core::prelude::DebugControl;
use ranvier_core::schematic::{NodeKind, Schematic};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[async_trait]
pub trait StateInspector: Send + Sync {
    async fn get_state(&self, trace_id: &str) -> Option<Value>;
    async fn force_resume(
        &self,
        trace_id: &str,
        target_node: &str,
        payload_override: Option<Value>,
    ) -> Result<(), String>;
}
use tokio::sync::broadcast;
use tower_http::cors::CorsLayer;
use tracing::{Event, Id, Subscriber};
use tracing_subscriber::{Layer, layer::Context, registry::LookupSpan};

static EVENT_CHANNEL: OnceLock<broadcast::Sender<String>> = OnceLock::new();
static TRACE_REGISTRY: OnceLock<Arc<Mutex<ActiveTraceRegistry>>> = OnceLock::new();
static PAYLOAD_POLICY: OnceLock<payload::PayloadCapturePolicy> = OnceLock::new();
const QUICK_VIEW_HTML: &str = include_str!("quick_view/index.html");
const QUICK_VIEW_JS: &str = include_str!("quick_view/app.js");
const QUICK_VIEW_CSS: &str = include_str!("quick_view/styles.css");
const INSPECTOR_API_VERSION: &str = "1.0";

#[derive(Clone, Debug, serde::Serialize)]
pub struct TraceRecord {
    pub trace_id: String,
    pub circuit: String,
    pub status: TraceStatus,
    pub started_at: u64,
    pub finished_at: Option<u64>,
    pub duration_ms: Option<u64>,
    pub outcome_type: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TraceStatus {
    Active,
    Completed,
    Faulted,
}

pub struct ActiveTraceRegistry {
    active: HashMap<String, TraceRecord>,
    recent: VecDeque<TraceRecord>,
    max_recent: usize,
    /// TTL in milliseconds. Traces older than this are pruned on insertion.
    trace_ttl_ms: u64,
}

impl ActiveTraceRegistry {
    fn new() -> Self {
        Self {
            active: HashMap::new(),
            recent: VecDeque::new(),
            max_recent: 10_000,
            trace_ttl_ms: 3_600_000, // 1 hour default
        }
    }

    fn with_config(max_recent: usize, trace_ttl_ms: u64) -> Self {
        Self {
            active: HashMap::new(),
            recent: VecDeque::new(),
            max_recent,
            trace_ttl_ms,
        }
    }

    fn register(&mut self, circuit: String) {
        let trace_id = format!(
            "{}-{}",
            circuit.replace(' ', "_").to_lowercase(),
            epoch_ms()
        );
        self.active.insert(
            trace_id.clone(),
            TraceRecord {
                trace_id,
                circuit,
                status: TraceStatus::Active,
                started_at: epoch_ms(),
                finished_at: None,
                duration_ms: None,
                outcome_type: None,
            },
        );
    }

    fn complete(
        &mut self,
        circuit: &str,
        outcome_type: Option<String>,
        duration_ms: Option<u64>,
    ) {
        // Find the active trace for this circuit (most recent)
        let key = self
            .active
            .iter()
            .filter(|(_, r)| r.circuit == circuit)
            .max_by_key(|(_, r)| r.started_at)
            .map(|(k, _)| k.clone());

        if let Some(key) = key {
            if let Some(mut record) = self.active.remove(&key) {
                record.finished_at = Some(epoch_ms());
                record.duration_ms = duration_ms;
                record.outcome_type = outcome_type.clone();
                record.status = if outcome_type.as_deref() == Some("Fault") {
                    TraceStatus::Faulted
                } else {
                    TraceStatus::Completed
                };

                // Prune expired traces before inserting
                self.prune_expired();

                self.recent.push_back(record);
                while self.recent.len() > self.max_recent {
                    self.recent.pop_front();
                }
            }
        }
    }

    /// Remove traces that have exceeded the TTL.
    fn prune_expired(&mut self) {
        if self.trace_ttl_ms == 0 {
            return;
        }
        let cutoff = epoch_ms().saturating_sub(self.trace_ttl_ms);
        while let Some(front) = self.recent.front() {
            if front.started_at < cutoff {
                self.recent.pop_front();
            } else {
                break;
            }
        }
    }

    fn list_all(&self) -> Vec<TraceRecord> {
        let mut result: Vec<TraceRecord> = self.active.values().cloned().collect();
        result.extend(self.recent.iter().cloned());
        result.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        result
    }

    /// Number of currently active (in-flight) traces.
    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    /// Number of recently completed traces in the ring buffer.
    pub fn recent_count(&self) -> usize {
        self.recent.len()
    }
}

pub fn get_trace_registry() -> Arc<Mutex<ActiveTraceRegistry>> {
    TRACE_REGISTRY
        .get_or_init(|| Arc::new(Mutex::new(ActiveTraceRegistry::new())))
        .clone()
}

/// Initialize the trace registry with custom configuration.
/// Must be called before `get_trace_registry()` for config to take effect.
fn init_trace_registry(config: &TraceRegistryConfig) {
    let _ = TRACE_REGISTRY.get_or_init(|| {
        Arc::new(Mutex::new(ActiveTraceRegistry::with_config(
            config.max_traces,
            config.trace_ttl.as_millis() as u64,
        )))
    });
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InspectorMode {
    Dev,
    Prod,
}

impl InspectorMode {
    fn from_env() -> Self {
        match std::env::var("RANVIER_MODE")
            .unwrap_or_else(|_| "dev".to_string())
            .to_ascii_lowercase()
            .as_str()
        {
            "prod" | "production" => Self::Prod,
            _ => Self::Dev,
        }
    }

    fn from_str(mode: &str) -> Self {
        match mode.to_ascii_lowercase().as_str() {
            "prod" | "production" => Self::Prod,
            _ => Self::Dev,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct SurfacePolicy {
    expose_internal: bool,
    expose_events: bool,
    expose_quick_view: bool,
}

impl SurfacePolicy {
    fn for_mode(mode: InspectorMode) -> Self {
        match mode {
            InspectorMode::Dev => Self {
                expose_internal: true,
                expose_events: true,
                expose_quick_view: true,
            },
            InspectorMode::Prod => Self {
                expose_internal: false,
                expose_events: false,
                expose_quick_view: false,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AccessRole {
    Viewer,
    Operator,
    Admin,
}

#[derive(Clone, Copy, Debug)]
struct AuthPolicy {
    enforce_headers: bool,
    require_tenant_for_internal: bool,
}

impl AuthPolicy {
    fn default() -> Self {
        Self {
            enforce_headers: false,
            require_tenant_for_internal: false,
        }
    }

    fn from_env() -> Self {
        Self {
            enforce_headers: env_flag("RANVIER_AUTH_ENFORCE", false),
            require_tenant_for_internal: env_flag("RANVIER_AUTH_REQUIRE_TENANT_INTERNAL", false),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProjectionSurface {
    Public,
    Internal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RedactionMode {
    Off,
    Public,
    Strict,
}

impl RedactionMode {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "public" => Some(Self::Public),
            "strict" => Some(Self::Strict),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
struct TelemetryRedactionPolicy {
    mode_override: Option<RedactionMode>,
    sensitive_patterns: Vec<String>,
    allow_keys: HashSet<String>,
}

impl Default for TelemetryRedactionPolicy {
    fn default() -> Self {
        Self {
            mode_override: None,
            sensitive_patterns: default_sensitive_patterns(),
            allow_keys: HashSet::new(),
        }
    }
}

impl TelemetryRedactionPolicy {
    fn from_env() -> Self {
        let mut policy = Self::default();

        if let Ok(raw_mode) = std::env::var("RANVIER_TELEMETRY_REDACT_MODE") {
            match RedactionMode::parse(&raw_mode) {
                Some(mode) => policy.mode_override = Some(mode),
                None => tracing::warn!(
                    "Invalid RANVIER_TELEMETRY_REDACT_MODE='{}' (expected: off|public|strict)",
                    raw_mode
                ),
            }
        }

        if let Ok(extra) = std::env::var("RANVIER_TELEMETRY_REDACT_KEYS") {
            for key in parse_csv_lower(&extra) {
                if !policy.sensitive_patterns.contains(&key) {
                    policy.sensitive_patterns.push(key);
                }
            }
        }

        if let Ok(allow) = std::env::var("RANVIER_TELEMETRY_ALLOW_KEYS") {
            policy.allow_keys.extend(parse_csv_lower(&allow));
        }

        policy
    }

    fn mode_for_surface(&self, surface: ProjectionSurface) -> RedactionMode {
        if let Some(mode) = self.mode_override {
            return mode;
        }

        match surface {
            ProjectionSurface::Public => RedactionMode::Public,
            ProjectionSurface::Internal => RedactionMode::Off,
        }
    }

    fn is_sensitive_key(&self, key: &str) -> bool {
        let lowered = key.to_ascii_lowercase();
        self.sensitive_patterns
            .iter()
            .any(|pattern| lowered.contains(pattern))
    }
}

fn get_sender() -> &'static broadcast::Sender<String> {
    EVENT_CHANNEL.get_or_init(|| {
        let (tx, _rx) = broadcast::channel(100);
        tx
    })
}

/// Configuration for the in-memory trace registry ring buffer.
#[derive(Clone, Debug)]
pub struct TraceRegistryConfig {
    /// Maximum number of recent traces to keep in the ring buffer.
    /// Default: 10,000.
    pub max_traces: usize,
    /// Time-to-live for traces in the ring buffer.
    /// Traces older than this are pruned on insertion.
    /// Default: 1 hour.
    pub trace_ttl: std::time::Duration,
}

impl Default for TraceRegistryConfig {
    fn default() -> Self {
        Self {
            max_traces: 10_000,
            trace_ttl: std::time::Duration::from_secs(3600),
        }
    }
}

/// Start the Inspector Server.
pub struct Inspector {
    port: u16,
    schematic: Arc<Mutex<Schematic>>,
    public_projection: Arc<Mutex<Option<Value>>>,
    internal_projection: Arc<Mutex<Option<Value>>>,
    public_projection_path: Option<String>,
    internal_projection_path: Option<String>,
    surface_policy: SurfacePolicy,
    auth_policy: AuthPolicy,
    redaction_policy: TelemetryRedactionPolicy,
    state_inspector: Option<Arc<dyn StateInspector>>,
    dlq_reader: Option<Arc<dyn DlqReader>>,
    payload_policy: payload::PayloadCapturePolicy,
    relay_state: Option<relay::RelayState>,
    bearer_auth: auth::BearerAuth,
    allow_unauthenticated: bool,
    trace_registry_config: TraceRegistryConfig,
    trace_store: Option<Arc<dyn trace_store::TraceStore>>,
    alert_dispatcher: Option<Arc<alert::AlertDispatcher>>,
}

impl Inspector {
    pub fn new(schematic: Schematic, port: u16) -> Self {
        // Ensure channel exists
        get_sender();
        let public_projection = default_public_projection(&schematic);
        let internal_projection = default_internal_projection(&schematic);

        Self {
            port,
            schematic: Arc::new(Mutex::new(schematic)),
            public_projection: Arc::new(Mutex::new(Some(public_projection))),
            internal_projection: Arc::new(Mutex::new(Some(internal_projection))),
            public_projection_path: None,
            internal_projection_path: None,
            surface_policy: SurfacePolicy::for_mode(InspectorMode::Dev),
            auth_policy: AuthPolicy::default(),
            redaction_policy: TelemetryRedactionPolicy::from_env(),
            state_inspector: None,
            dlq_reader: None,
            payload_policy: payload::PayloadCapturePolicy::from_env(),
            relay_state: None,
            bearer_auth: auth::BearerAuth::default(),
            allow_unauthenticated: false,
            trace_registry_config: TraceRegistryConfig::default(),
            trace_store: None,
            alert_dispatcher: None,
        }
    }

    /// Configure the relay target for proxying API requests into the running application.
    ///
    /// When set, `/api/v1/relay` will forward requests to the specified URL via `reqwest`.
    ///
    /// ```rust,ignore
    /// let inspector = Inspector::new(schematic, 9090)
    ///     .with_relay_target("http://127.0.0.1:3111");
    /// ```
    pub fn with_relay_target(mut self, target_url: impl Into<String>) -> Self {
        let config = relay::RelayConfig::new(target_url);
        self.relay_state = Some(relay::RelayState::new(config));
        self
    }

    /// Register HTTP route descriptors for the `/api/v1/routes` endpoint.
    ///
    /// Call this with the route descriptors from your `HttpIngress`.
    pub fn with_routes(self, route_infos: Vec<routes::RouteInfo>) -> Self {
        routes::register_routes(route_infos);
        self
    }

    pub fn with_dlq_reader(mut self, reader: Arc<dyn DlqReader>) -> Self {
        self.dlq_reader = Some(reader);
        self
    }

    pub fn with_payload_capture(mut self, policy: payload::PayloadCapturePolicy) -> Self {
        self.payload_policy = policy;
        self
    }

    pub fn with_state_inspector(mut self, inspector: Arc<dyn StateInspector>) -> Self {
        self.state_inspector = Some(inspector);
        self
    }

    /// Attach a read-only public projection artifact.
    pub fn with_public_projection(self, projection: Value) -> Self {
        if let Ok(mut slot) = self.public_projection.lock() {
            *slot = Some(projection);
        }
        self
    }

    /// Attach a read-only internal projection artifact.
    pub fn with_internal_projection(self, projection: Value) -> Self {
        if let Ok(mut slot) = self.internal_projection.lock() {
            *slot = Some(projection);
        }
        self
    }

    /// Load optional projection artifacts from environment variables:
    /// - `RANVIER_TRACE_PUBLIC_PATH`
    /// - `RANVIER_TRACE_INTERNAL_PATH`
    ///
    /// Invalid files are ignored with warning logs; bootstrap projections remain active.
    pub fn with_projection_files_from_env(self) -> Self {
        let mut inspector = self;

        if let Ok(path) = std::env::var("RANVIER_TRACE_PUBLIC_PATH") {
            inspector.public_projection_path = Some(path.clone());
            match read_projection_file(&path) {
                Ok(v) => inspector = inspector.with_public_projection(v),
                Err(e) => tracing::warn!("Failed to load public projection from {}: {}", path, e),
            }
        }

        if let Ok(path) = std::env::var("RANVIER_TRACE_INTERNAL_PATH") {
            inspector.internal_projection_path = Some(path.clone());
            match read_projection_file(&path) {
                Ok(v) => inspector = inspector.with_internal_projection(v),
                Err(e) => tracing::warn!("Failed to load internal projection from {}: {}", path, e),
            }
        }

        inspector
    }

    /// Configure inspector route surface using `RANVIER_MODE=dev|prod`.
    ///
    /// - `dev` (default): expose `/trace/internal`, `/events`, `/quick-view`
    /// - `prod`: hide internal/event/quick-view routes and keep public read-only endpoints
    pub fn with_mode_from_env(mut self) -> Self {
        let mode = InspectorMode::from_env();
        self.surface_policy = SurfacePolicy::for_mode(mode);
        self
    }

    /// Configure inspector route surface explicitly.
    ///
    /// Accepted values:
    /// - `dev` (default)
    /// - `prod` / `production`
    pub fn with_mode(mut self, mode: &str) -> Self {
        let parsed = InspectorMode::from_str(mode);
        self.surface_policy = SurfacePolicy::for_mode(parsed);
        self
    }

    /// Configure auth policy using environment variables.
    ///
    /// - `RANVIER_AUTH_ENFORCE=1`: require `X-Ranvier-Role` on inspector endpoints.
    /// - `RANVIER_AUTH_REQUIRE_TENANT_INTERNAL=1`: require `X-Ranvier-Tenant` for internal endpoints.
    pub fn with_auth_policy_from_env(mut self) -> Self {
        self.auth_policy = AuthPolicy::from_env();
        self
    }

    /// Toggle role-header enforcement.
    pub fn with_auth_enforcement(mut self, enabled: bool) -> Self {
        self.auth_policy.enforce_headers = enabled;
        self
    }

    /// Toggle tenant-header requirement for internal endpoints.
    pub fn with_require_tenant_for_internal(mut self, required: bool) -> Self {
        self.auth_policy.require_tenant_for_internal = required;
        self
    }

    /// Reload telemetry redaction policy from environment variables.
    ///
    /// Variables:
    /// - `RANVIER_TELEMETRY_REDACT_MODE=off|public|strict`
    /// - `RANVIER_TELEMETRY_REDACT_KEYS=comma,separated,patterns`
    /// - `RANVIER_TELEMETRY_ALLOW_KEYS=comma,separated,keys`
    pub fn with_redaction_policy_from_env(mut self) -> Self {
        self.redaction_policy = TelemetryRedactionPolicy::from_env();
        self
    }

    /// Configure Bearer token authentication for production deployments.
    pub fn with_bearer_token(mut self, token: impl Into<String>) -> Self {
        self.bearer_auth = auth::BearerAuth { token: Some(token.into()) };
        self
    }

    /// Load bearer token from `RANVIER_INSPECTOR_TOKEN` environment variable.
    pub fn with_bearer_token_from_env(mut self) -> Self {
        self.bearer_auth = auth::BearerAuth::from_env();
        self
    }

    /// Explicitly allow running the Inspector without Bearer token authentication.
    ///
    /// In release builds, the Inspector will emit a warning at startup if no bearer
    /// token is configured. Call this method to suppress that warning and acknowledge
    /// that the Inspector is intentionally running without authentication.
    ///
    /// **Not recommended for production deployments.** Use `with_bearer_token()` or
    /// `with_bearer_token_from_env()` instead.
    pub fn allow_unauthenticated(mut self) -> Self {
        self.allow_unauthenticated = true;
        self
    }

    /// Configure a persistent trace store for trace history.
    pub fn with_trace_store(mut self, store: Arc<dyn trace_store::TraceStore>) -> Self {
        self.trace_store = Some(store);
        self
    }

    /// Configure the in-memory trace registry ring buffer.
    ///
    /// Controls the maximum number of recent traces kept and their TTL.
    /// Default: max_traces=10,000, trace_ttl=1h.
    ///
    /// ```rust,ignore
    /// let inspector = Inspector::new(schematic, 9090)
    ///     .with_trace_registry_config(TraceRegistryConfig {
    ///         max_traces: 5_000,
    ///         trace_ttl: std::time::Duration::from_secs(1800), // 30 minutes
    ///     });
    /// ```
    pub fn with_trace_registry_config(mut self, config: TraceRegistryConfig) -> Self {
        self.trace_registry_config = config;
        self
    }

    /// Configure alert hooks for production monitoring.
    pub fn with_alert_dispatcher(mut self, dispatcher: Arc<alert::AlertDispatcher>) -> Self {
        self.alert_dispatcher = Some(dispatcher);
        self
    }

    pub async fn serve(self) -> Result<(), std::io::Error> {
        let addr = SocketAddr::from(([0, 0, 0, 0], self.port));
        let listener = tokio::net::TcpListener::bind(addr).await?;
        self.serve_with_listener(listener).await
    }

    pub async fn serve_with_listener(
        self,
        listener: tokio::net::TcpListener,
    ) -> Result<(), std::io::Error> {
        // Auth policy enforcement: warn in release builds if no bearer token configured
        if !self.bearer_auth.is_enabled() && !self.allow_unauthenticated {
            #[cfg(not(debug_assertions))]
            tracing::warn!(
                "Inspector started WITHOUT Bearer token authentication. \
                 This is a security risk in production. Configure authentication via \
                 Inspector::with_bearer_token() or Inspector::with_bearer_token_from_env(). \
                 To suppress this warning, call Inspector::allow_unauthenticated()."
            );
            #[cfg(debug_assertions)]
            tracing::info!(
                "Inspector running without authentication (debug build). \
                 For production, configure Bearer token via with_bearer_token()."
            );
        }

        // Initialize trace registry with configured limits
        init_trace_registry(&self.trace_registry_config);

        let state = InspectorState {
            schematic: self.schematic.clone(),
            public_projection: self.public_projection.clone(),
            internal_projection: self.internal_projection.clone(),
            public_projection_path: self.public_projection_path.clone(),
            internal_projection_path: self.internal_projection_path.clone(),
            auth_policy: self.auth_policy,
            redaction_policy: self.redaction_policy.clone(),
            state_inspector: self.state_inspector,
            dlq_reader: self.dlq_reader,
            relay_state: self.relay_state,
            bearer_auth: self.bearer_auth,
            trace_store: self.trace_store,
            alert_dispatcher: self.alert_dispatcher,
        };

        // Store payload capture policy in a global for the tracing Layer to access
        PAYLOAD_POLICY.get_or_init(|| self.payload_policy);

        let mut app = Router::new()
            .route("/schematic", get(get_schematic))
            .route("/trace/public", get(get_public_projection))
            .route("/debug/resume/:trace_id", get(debug_resume))
            .route("/debug/step/:trace_id", get(debug_step))
            .route("/debug/pause/:trace_id", get(debug_pause))
            .route("/api/v1/state/:trace_id", get(api_get_state))
            .route(
                "/api/v1/state/:trace_id/resume",
                axum::routing::post(api_post_resume),
            )
            .route("/metrics", get(prometheus_metrics_handler))
            .layer(CorsLayer::permissive());

        if self.surface_policy.expose_internal {
            app = app
                .route("/trace/internal", get(get_internal_projection))
                .route("/inspector/circuits", get(get_inspector_circuits))
                .route(
                    "/inspector/circuits/:name",
                    get(get_inspector_circuit_by_name),
                )
                .route("/inspector/bus", get(get_inspector_bus))
                .route(
                    "/inspector/timeline/:request_id",
                    get(get_inspector_timeline_by_request_id),
                )
                .route("/api/v1/traces", get(api_get_traces))
                .route("/api/v1/metrics", get(api_get_metrics_all))
                .route("/api/v1/metrics/:circuit", get(api_get_metrics))
                .route("/api/v1/events", get(api_get_events))
                .route("/api/v1/dlq", get(api_get_dlq))
                .route(
                    "/api/v1/breakpoints",
                    get(api_get_breakpoints).post(api_post_breakpoint),
                )
                .route(
                    "/api/v1/breakpoints/:bp_id",
                    axum::routing::delete(api_delete_breakpoint)
                        .patch(api_patch_breakpoint),
                )
                .route("/api/v1/stalls", get(api_get_stalls))
                .route("/api/v1/routes", get(api_get_routes))
                .route(
                    "/api/v1/routes/schema",
                    axum::routing::post(api_post_routes_schema),
                )
                .route(
                    "/api/v1/routes/sample",
                    axum::routing::post(api_post_routes_sample),
                )
                .route(
                    "/api/v1/relay",
                    axum::routing::post(api_post_relay),
                )
                .route("/api/v1/traces/stored", get(api_get_stored_traces))
                .route("/api/v1/lineage/:trace_id", get(api_get_lineage))
                .route("/api/v1/traces/diff", get(api_get_trace_diff));
        }

        if self.surface_policy.expose_events {
            app = app.route("/events", get(ws_handler));
        }

        if self.surface_policy.expose_quick_view {
            app = app
                .route("/quick-view", get(get_quick_view_html))
                .route("/quick-view/app.js", get(get_quick_view_js))
                .route("/quick-view/styles.css", get(get_quick_view_css));
        }

        let app = app.with_state(state);
        let addr = listener.local_addr()?;
        tracing::info!("Ranvier Inspector listening on http://{}", addr);

        // Spawn periodic metrics broadcast task
        if self.surface_policy.expose_events {
            let broadcast_interval = std::env::var("RANVIER_INSPECTOR_METRICS_INTERVAL_MS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(2000);
            tokio::spawn(async move {
                let mut interval =
                    tokio::time::interval(std::time::Duration::from_millis(broadcast_interval));
                loop {
                    interval.tick().await;
                    let snapshots = metrics::snapshot_all();
                    if !snapshots.is_empty() {
                        let msg = serde_json::json!({
                            "type": "metrics",
                            "circuits": snapshots,
                            "timestamp": epoch_ms()
                        })
                        .to_string();
                        let _ = get_sender().send(msg);
                    }

                    // Stall detection piggybacks on the same timer
                    let stalls = stall::detect_stalls();
                    if !stalls.is_empty() {
                        let msg = serde_json::json!({
                            "type": "stall_detected",
                            "stalls": stalls,
                            "timestamp": epoch_ms()
                        })
                        .to_string();
                        let _ = get_sender().send(msg);
                    }
                }
            });
        }

        axum::serve(listener, app).await
    }
}

fn default_public_projection(schematic: &Schematic) -> Value {
    serde_json::json!({
        "service_name": schematic.name,
        "window_start": "1970-01-01T00:00:00Z",
        "window_end": "1970-01-01T00:00:00Z",
        "overall_status": "operational",
        "circuits": [
            {
                "name": schematic.name,
                "status": "operational",
                "success_rate": 1.0,
                "error_rate": 0.0,
                "p95_latency_ms": 0.0
            }
        ]
    })
}

fn default_internal_projection(schematic: &Schematic) -> Value {
    let nodes = schematic
        .nodes
        .iter()
        .map(|n| {
            serde_json::json!({
                "node_id": n.id,
                "label": n.label,
                "kind": node_kind_name(&n.kind),
                "entered_at": "1970-01-01T00:00:00Z",
                "exited_at": "1970-01-01T00:00:00Z",
                "latency_ms": 0.0,
                "outcome_type": "Next",
                "branch_id": Value::Null,
                "error_code": Value::Null,
                "error_category": Value::Null
            })
        })
        .collect::<Vec<_>>();

    serde_json::json!({
        "trace_id": "bootstrap",
        "circuit_id": schematic.id,
        "started_at": "1970-01-01T00:00:00Z",
        "finished_at": "1970-01-01T00:00:00Z",
        "nodes": nodes,
        "summary": {
            "node_count": schematic.nodes.len(),
            "fault_count": 0,
            "branch_count": 0
        }
    })
}

fn node_kind_name(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Ingress => "Ingress",
        NodeKind::Atom => "Atom",
        NodeKind::Synapse => "Synapse",
        NodeKind::Egress => "Egress",
        NodeKind::Subgraph(_) => "Subgraph",
        NodeKind::FanOut => "FanOut",
        NodeKind::FanIn => "FanIn",
        NodeKind::StreamingTransition => "StreamingTransition",
    }
}

fn read_projection_file(path: &str) -> Result<Value, String> {
    let content = fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str::<Value>(&content).map_err(|e| e.to_string())
}

fn default_sensitive_patterns() -> Vec<String> {
    vec![
        "password".to_string(),
        "secret".to_string(),
        "token".to_string(),
        "authorization".to_string(),
        "cookie".to_string(),
        "session".to_string(),
        "api_key".to_string(),
        "credit_card".to_string(),
        "ssn".to_string(),
        "email".to_string(),
        "phone".to_string(),
    ]
}

fn parse_csv_lower(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

fn apply_projection_redaction(
    projection: Value,
    surface: ProjectionSurface,
    policy: &TelemetryRedactionPolicy,
) -> Value {
    let mode = policy.mode_for_surface(surface);
    redact_json_value(projection, mode, policy, None)
}

fn redact_json_value(
    value: Value,
    mode: RedactionMode,
    policy: &TelemetryRedactionPolicy,
    parent_key: Option<&str>,
) -> Value {
    if mode == RedactionMode::Off {
        return value;
    }

    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            let strict_attribute_bag =
                mode == RedactionMode::Strict && parent_key.is_some_and(is_attribute_bag_key);

            for (key, child) in map {
                let lowered = key.to_ascii_lowercase();

                if strict_attribute_bag && !policy.allow_keys.contains(&lowered) {
                    if policy.is_sensitive_key(&lowered) {
                        out.insert(key, Value::String("[REDACTED]".to_string()));
                    }
                    continue;
                }

                if policy.is_sensitive_key(&lowered) {
                    out.insert(key, Value::String("[REDACTED]".to_string()));
                    continue;
                }

                out.insert(
                    key.clone(),
                    redact_json_value(child, mode, policy, Some(&key)),
                );
            }
            Value::Object(out)
        }
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(|v| redact_json_value(v, mode, policy, parent_key))
                .collect(),
        ),
        other => other,
    }
}

fn is_attribute_bag_key(key: &str) -> bool {
    let lowered = key.to_ascii_lowercase();
    lowered == "attributes" || lowered.ends_with("_attributes")
}

async fn debug_resume(AxPath(trace_id): AxPath<String>) -> impl IntoResponse {
    if let Some(debug) = get_debug_control_for_trace(&trace_id) {
        debug.resume();
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    }
}

async fn debug_step(AxPath(trace_id): AxPath<String>) -> impl IntoResponse {
    if let Some(debug) = get_debug_control_for_trace(&trace_id) {
        debug.step();
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    }
}

async fn debug_pause(AxPath(trace_id): AxPath<String>) -> impl IntoResponse {
    if let Some(debug) = get_debug_control_for_trace(&trace_id) {
        debug.pause();
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    }
}

async fn api_get_state(
    AxPath(trace_id): AxPath<String>,
    State(state): State<InspectorState>,
) -> impl IntoResponse {
    if let Some(inspector) = state.state_inspector.as_ref()
        && let Some(val) = inspector.get_state(&trace_id).await
    {
        return Json(val).into_response();
    }
    StatusCode::NOT_FOUND.into_response()
}

#[derive(Deserialize)]
struct ResumePayload {
    target_node: String,
    #[allow(dead_code)]
    force: bool,
    payload_override: Option<Value>,
}

async fn api_post_resume(
    AxPath(trace_id): AxPath<String>,
    State(state): State<InspectorState>,
    Json(payload): Json<ResumePayload>,
) -> impl IntoResponse {
    if let Some(inspector) = state.state_inspector.as_ref() {
        match inspector
            .force_resume(&trace_id, &payload.target_node, payload.payload_override)
            .await
        {
            Ok(_) => StatusCode::OK.into_response(),
            Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
        }
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "State inspector not configured",
        )
            .into_response()
    }
}

async fn api_get_traces(
    headers: HeaderMap,
    State(state): State<InspectorState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    ensure_internal_access(&headers, &state.auth_policy)?;
    let traces = get_trace_registry()
        .lock()
        .map(|r| r.list_all())
        .unwrap_or_default();
    Ok(inspector_envelope(
        "inspector.traces.v1",
        serde_json::json!({
            "count": traces.len(),
            "traces": traces
        }),
    ))
}

async fn api_get_metrics(
    headers: HeaderMap,
    AxPath(circuit): AxPath<String>,
    State(state): State<InspectorState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    ensure_internal_access(&headers, &state.auth_policy)?;
    match metrics::snapshot_circuit(&circuit) {
        Some(snap) => Ok(inspector_envelope(
            "inspector.metrics.v1",
            serde_json::json!(snap),
        )),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "No metrics for circuit", "circuit": circuit })),
        )),
    }
}

async fn api_get_metrics_all(
    headers: HeaderMap,
    State(state): State<InspectorState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    ensure_internal_access(&headers, &state.auth_policy)?;
    let snapshots = metrics::snapshot_all();
    Ok(inspector_envelope(
        "inspector.metrics.v1",
        serde_json::json!({
            "count": snapshots.len(),
            "circuits": snapshots
        }),
    ))
}

/// Prometheus exposition format endpoint — `GET /metrics`.
///
/// Protected by BearerAuth when configured. Returns per-node execution
/// metrics (invocations, errors, throughput, latency percentiles) and
/// active trace count in Prometheus text format.
async fn prometheus_metrics_handler(
    headers: HeaderMap,
    State(state): State<InspectorState>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    state.bearer_auth.validate(&headers)?;
    let body = prometheus::render();
    Ok((
        [(header::CONTENT_TYPE, prometheus::CONTENT_TYPE)],
        body,
    ))
}

async fn api_get_events(
    headers: HeaderMap,
    State(state): State<InspectorState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    ensure_internal_access(&headers, &state.auth_policy)?;
    let events = payload::list_events(200);
    Ok(inspector_envelope(
        "inspector.events.v1",
        serde_json::json!({
            "count": events.len(),
            "events": events
        }),
    ))
}

async fn api_get_dlq(
    headers: HeaderMap,
    State(state): State<InspectorState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    ensure_internal_access(&headers, &state.auth_policy)?;
    match &state.dlq_reader {
        Some(reader) => {
            let letters = reader
                .list_dead_letters(None, 100)
                .await
                .unwrap_or_default();
            let count = reader.count_dead_letters().await.unwrap_or(0);
            Ok(inspector_envelope(
                "inspector.dlq.v1",
                serde_json::json!({
                    "total": count,
                    "items": letters
                }),
            ))
        }
        None => Ok(inspector_envelope(
            "inspector.dlq.v1",
            serde_json::json!({
                "total": 0,
                "items": [],
                "note": "No DLQ reader configured"
            }),
        )),
    }
}

async fn api_get_breakpoints(
    headers: HeaderMap,
    State(state): State<InspectorState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    ensure_internal_access(&headers, &state.auth_policy)?;
    let bps = breakpoint::list_breakpoints();
    Ok(inspector_envelope(
        "inspector.breakpoints.v1",
        serde_json::json!({
            "count": bps.len(),
            "breakpoints": bps
        }),
    ))
}

#[derive(Deserialize)]
struct CreateBreakpointPayload {
    node_id: String,
    condition: Option<String>,
}

async fn api_post_breakpoint(
    headers: HeaderMap,
    State(state): State<InspectorState>,
    Json(body): Json<CreateBreakpointPayload>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    ensure_internal_access(&headers, &state.auth_policy)?;
    let bp = breakpoint::add_breakpoint(body.node_id, body.condition);
    Ok((
        StatusCode::CREATED,
        inspector_envelope("inspector.breakpoint.v1", serde_json::json!(bp)),
    ))
}

async fn api_delete_breakpoint(
    headers: HeaderMap,
    AxPath(bp_id): AxPath<String>,
    State(state): State<InspectorState>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    ensure_internal_access(&headers, &state.auth_policy)?;
    if breakpoint::remove_breakpoint(&bp_id) {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(policy_error(StatusCode::NOT_FOUND, "breakpoint_not_found"))
    }
}

#[derive(Deserialize)]
struct PatchBreakpointPayload {
    enabled: Option<bool>,
    condition: Option<Option<String>>,
}

async fn api_patch_breakpoint(
    headers: HeaderMap,
    AxPath(bp_id): AxPath<String>,
    State(state): State<InspectorState>,
    Json(body): Json<PatchBreakpointPayload>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    ensure_internal_access(&headers, &state.auth_policy)?;
    match breakpoint::update_breakpoint(&bp_id, body.enabled, body.condition) {
        Some(bp) => Ok(inspector_envelope(
            "inspector.breakpoint.v1",
            serde_json::json!(bp),
        )),
        None => Err(policy_error(StatusCode::NOT_FOUND, "breakpoint_not_found")),
    }
}

async fn api_get_stalls(
    headers: HeaderMap,
    State(state): State<InspectorState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    ensure_internal_access(&headers, &state.auth_policy)?;
    let stalls = stall::detect_stalls();
    Ok(inspector_envelope(
        "inspector.stalls.v1",
        serde_json::json!({
            "count": stalls.len(),
            "stalls": stalls
        }),
    ))
}

static DEBUG_REGISTRY: OnceLock<Arc<Mutex<HashMap<String, DebugControl>>>> = OnceLock::new();

fn get_debug_registry() -> Arc<Mutex<HashMap<String, DebugControl>>> {
    DEBUG_REGISTRY
        .get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
        .clone()
}

pub fn get_debug_control_for_trace(trace_id: &str) -> Option<DebugControl> {
    get_debug_registry().lock().unwrap().get(trace_id).cloned()
}

pub fn register_debug_control(trace_id: String, control: DebugControl) {
    get_debug_registry()
        .lock()
        .unwrap()
        .insert(trace_id, control);
}

pub fn unregister_debug_control(trace_id: &str) {
    get_debug_registry().lock().unwrap().remove(trace_id);
}

#[derive(Clone)]
struct InspectorState {
    schematic: Arc<Mutex<Schematic>>,
    public_projection: Arc<Mutex<Option<Value>>>,
    internal_projection: Arc<Mutex<Option<Value>>>,
    public_projection_path: Option<String>,
    internal_projection_path: Option<String>,
    auth_policy: AuthPolicy,
    redaction_policy: TelemetryRedactionPolicy,
    state_inspector: Option<Arc<dyn StateInspector>>,
    dlq_reader: Option<Arc<dyn DlqReader>>,
    relay_state: Option<relay::RelayState>,
    bearer_auth: auth::BearerAuth,
    trace_store: Option<Arc<dyn trace_store::TraceStore>>,
    #[allow(dead_code)]
    alert_dispatcher: Option<Arc<alert::AlertDispatcher>>,
}

pub fn layer() -> InspectorLayer {
    InspectorLayer
}

/// Per-span data stored in span extensions by InspectorLayer.
struct SpanData {
    node_id: Option<String>,
    resource_type: Option<String>,
    circuit: Option<String>,
    outcome_kind: Option<String>,
    outcome_target: Option<String>,
    entered_at: Option<Instant>,
    duration_ms: Option<u64>,
}

impl SpanData {
    fn from_visitor(v: SpanFieldExtractor) -> Self {
        Self {
            node_id: v.node_id,
            resource_type: v.resource_type,
            circuit: v.circuit,
            outcome_kind: v.outcome_kind,
            outcome_target: v.outcome_target,
            entered_at: None,
            duration_ms: None,
        }
    }

    fn update_from_visitor(&mut self, v: SpanFieldExtractor) {
        if let Some(val) = v.node_id {
            self.node_id = Some(val);
        }
        if let Some(val) = v.resource_type {
            self.resource_type = Some(val);
        }
        if let Some(val) = v.circuit {
            self.circuit = Some(val);
        }
        if let Some(val) = v.outcome_kind {
            self.outcome_kind = Some(val);
        }
        if let Some(val) = v.outcome_target {
            self.outcome_target = Some(val);
        }
    }
}

struct SpanFieldExtractor {
    node_id: Option<String>,
    resource_type: Option<String>,
    circuit: Option<String>,
    outcome_kind: Option<String>,
    outcome_target: Option<String>,
}

impl SpanFieldExtractor {
    fn new() -> Self {
        Self {
            node_id: None,
            resource_type: None,
            circuit: None,
            outcome_kind: None,
            outcome_target: None,
        }
    }
}

impl tracing::field::Visit for SpanFieldExtractor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "ranvier.node" => self.node_id = Some(value.to_string()),
            "ranvier.resource_type" => self.resource_type = Some(value.to_string()),
            "ranvier.circuit" => self.circuit = Some(value.to_string()),
            "ranvier.outcome_kind" => self.outcome_kind = Some(value.to_string()),
            "ranvier.outcome_target" => self.outcome_target = Some(value.to_string()),
            _ => {}
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        // For %Display fields, the Debug impl delegates to Display
        let s = format!("{value:?}");
        match field.name() {
            "ranvier.node" => self.node_id = Some(s),
            "ranvier.resource_type" => self.resource_type = Some(s),
            "ranvier.circuit" => self.circuit = Some(s),
            "ranvier.outcome_kind" => self.outcome_kind = Some(s),
            "ranvier.outcome_target" => self.outcome_target = Some(s),
            _ => {}
        }
    }
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub struct InspectorLayer;

impl<S> Layer<S> for InspectorLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &Id,
        ctx: Context<'_, S>,
    ) {
        if let Some(span) = ctx.span(id) {
            let name = span.name();
            if name == "Node" || name == "Circuit" || name == "NodeRetry" {
                let mut extractor = SpanFieldExtractor::new();
                attrs.record(&mut extractor);
                span.extensions_mut()
                    .insert(SpanData::from_visitor(extractor));
            }
        }
    }

    fn on_record(
        &self,
        id: &Id,
        values: &tracing::span::Record<'_>,
        ctx: Context<'_, S>,
    ) {
        if let Some(span) = ctx.span(id) {
            let name = span.name();
            if name == "Node" || name == "Circuit" || name == "NodeRetry" {
                let mut extractor = SpanFieldExtractor::new();
                values.record(&mut extractor);
                if let Some(data) = span.extensions_mut().get_mut::<SpanData>() {
                    data.update_from_visitor(extractor);
                }
            }
        }
    }

    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        if metadata.target() == "ranvier.debugger" {
            let mut fields = HashMap::new();
            let mut visitor = FieldVisitor {
                fields: &mut fields,
            };
            event.record(&mut visitor);

            if let (Some(trace_id), Some(node_id)) =
                (fields.get("trace_id"), fields.get("node_id"))
            {
                let msg = serde_json::json!({
                    "type": "node_paused",
                    "trace_id": trace_id,
                    "node_id": node_id,
                    "timestamp": epoch_ms()
                })
                .to_string();
                let _ = get_sender().send(msg);
            }
            return;
        }

        if metadata.target().starts_with("ranvier") {
            let mut fields = HashMap::new();
            let mut visitor = FieldVisitor {
                fields: &mut fields,
            };
            event.record(&mut visitor);

            let msg = serde_json::json!({
                "type": "event",
                "target": metadata.target(),
                "level": format!("{}", metadata.level()),
                "fields": fields,
                "timestamp": epoch_ms()
            })
            .to_string();
            let _ = get_sender().send(msg);
        }
    }

    fn on_enter(&self, id: &Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            let name = span.name();
            if name == "Node" || name == "Circuit" {
                let mut extensions = span.extensions_mut();
                if let Some(data) = extensions.get_mut::<SpanData>() {
                    data.entered_at = Some(Instant::now());

                    if name == "Node" {
                        let msg = serde_json::json!({
                            "type": "node_enter",
                            "node_id": data.node_id,
                            "resource_type": data.resource_type,
                            "timestamp": epoch_ms()
                        })
                        .to_string();
                        let _ = get_sender().send(msg);

                        // Register for stall detection
                        if let Some(node_id) = &data.node_id {
                            let circuit_name = data.circuit.clone().unwrap_or_default();
                            stall::register_node(
                                format!("{:?}", id),
                                node_id.clone(),
                                circuit_name,
                            );
                        }
                    } else if name == "Circuit" {
                        if let Some(circuit) = &data.circuit {
                            if let Ok(mut registry) = get_trace_registry().lock() {
                                registry.register(circuit.clone());
                            }
                        }
                    }
                }
            }
        }
    }

    fn on_exit(&self, id: &Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            let name = span.name();
            if name == "Node" || name == "Circuit" {
                let mut extensions = span.extensions_mut();
                if let Some(data) = extensions.get_mut::<SpanData>() {
                    // Store duration for use in on_close (outcome fields not yet recorded)
                    data.duration_ms =
                        data.entered_at.map(|t| t.elapsed().as_millis() as u64);
                }
            }
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(&id) {
            let name = span.name();
            if name == "Node" {
                // Unregister from stall detector
                stall::unregister_node(&format!("{:?}", id));

                let extensions = span.extensions();
                if let Some(data) = extensions.get::<SpanData>() {
                    let duration = data.duration_ms.unwrap_or(0);
                    let is_error = data.outcome_kind.as_deref() == Some("Fault");

                    let msg = serde_json::json!({
                        "type": "node_exit",
                        "node_id": data.node_id,
                        "resource_type": data.resource_type,
                        "outcome_type": data.outcome_kind,
                        "outcome_target": data.outcome_target,
                        "duration_ms": duration,
                        "timestamp": epoch_ms()
                    })
                    .to_string();
                    let _ = get_sender().send(msg);

                    // Record metrics — resolve parent circuit name
                    let circuit_name = span
                        .parent()
                        .and_then(|p| {
                            p.extensions()
                                .get::<SpanData>()
                                .and_then(|d| d.circuit.clone())
                        });
                    if let Some(node_id) = &data.node_id {
                        metrics::record_global_node_exit(
                            circuit_name.as_deref().unwrap_or("default"),
                            node_id,
                            duration,
                            is_error,
                        );
                    }

                    // Record event in ring buffer
                    payload::record_event(payload::CapturedEvent {
                        timestamp: epoch_ms(),
                        event_type: "node_exit".to_string(),
                        node_id: data.node_id.clone(),
                        circuit: circuit_name,
                        duration_ms: Some(duration),
                        outcome_type: data.outcome_kind.clone(),
                        payload_hash: None,
                        payload_json: None,
                    });
                }
            } else if name == "Circuit" {
                let extensions = span.extensions();
                if let Some(data) = extensions.get::<SpanData>() {
                    let msg = serde_json::json!({
                        "type": "circuit_exit",
                        "circuit": data.circuit,
                        "outcome_type": data.outcome_kind,
                        "outcome_target": data.outcome_target,
                        "duration_ms": data.duration_ms.unwrap_or(0),
                        "timestamp": epoch_ms()
                    })
                    .to_string();
                    let _ = get_sender().send(msg);

                    // Complete trace in registry
                    if let Some(circuit) = &data.circuit {
                        if let Ok(mut registry) = get_trace_registry().lock() {
                            registry.complete(
                                circuit,
                                data.outcome_kind.clone(),
                                data.duration_ms,
                            );
                        }
                    }

                    // Record circuit event
                    payload::record_event(payload::CapturedEvent {
                        timestamp: epoch_ms(),
                        event_type: "circuit_exit".to_string(),
                        node_id: None,
                        circuit: data.circuit.clone(),
                        duration_ms: data.duration_ms,
                        outcome_type: data.outcome_kind.clone(),
                        payload_hash: None,
                        payload_json: None,
                    });
                }
            }
        }
    }
}

struct FieldVisitor<'a> {
    fields: &'a mut HashMap<String, String>,
}

impl<'a> tracing::field::Visit for FieldVisitor<'a> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.fields
            .insert(field.name().to_string(), format!("{:?}", value));
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.fields
            .insert(field.name().to_string(), value.to_string());
    }
}

async fn get_schematic(
    headers: HeaderMap,
    State(state): State<InspectorState>,
) -> Result<Json<Schematic>, (StatusCode, Json<Value>)> {
    ensure_public_access(&headers, &state.auth_policy)?;
    let schematic = state.schematic.lock().unwrap();
    Ok(Json(schematic.clone()))
}

async fn get_public_projection(
    headers: HeaderMap,
    State(state): State<InspectorState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    ensure_public_access(&headers, &state.auth_policy)?;
    if let Some(path) = &state.public_projection_path
        && let Ok(v) = read_projection_file(path)
    {
        return Ok(Json(apply_projection_redaction(
            v,
            ProjectionSurface::Public,
            &state.redaction_policy,
        )));
    }

    let projection = state
        .public_projection
        .lock()
        .ok()
        .and_then(|v| v.clone())
        .unwrap_or(Value::Null);
    Ok(Json(apply_projection_redaction(
        projection,
        ProjectionSurface::Public,
        &state.redaction_policy,
    )))
}

async fn get_internal_projection(
    headers: HeaderMap,
    State(state): State<InspectorState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    ensure_internal_access(&headers, &state.auth_policy)?;
    if let Some(path) = &state.internal_projection_path
        && let Ok(v) = read_projection_file(path)
    {
        return Ok(Json(apply_projection_redaction(
            v,
            ProjectionSurface::Internal,
            &state.redaction_policy,
        )));
    }

    let projection = state
        .internal_projection
        .lock()
        .ok()
        .and_then(|v| v.clone())
        .unwrap_or(Value::Null);
    Ok(Json(apply_projection_redaction(
        projection,
        ProjectionSurface::Internal,
        &state.redaction_policy,
    )))
}

fn inspector_envelope(kind: &'static str, data: Value) -> Json<Value> {
    Json(serde_json::json!({
        "api_version": INSPECTOR_API_VERSION,
        "kind": kind,
        "data": data
    }))
}

fn load_internal_projection_value(state: &InspectorState) -> Value {
    if let Some(path) = &state.internal_projection_path
        && let Ok(v) = read_projection_file(path)
    {
        return v;
    }
    state
        .internal_projection
        .lock()
        .ok()
        .and_then(|v| v.clone())
        .unwrap_or(Value::Null)
}

fn latest_trace_from_projection(projection: &Value) -> Option<Value> {
    match projection {
        Value::Object(map) => {
            if let Some(Value::Array(traces)) = map.get("traces") {
                return traces.last().cloned();
            }
            if map.get("trace_id").is_some() {
                return Some(projection.clone());
            }
            None
        }
        Value::Array(traces) => traces.last().cloned(),
        _ => None,
    }
}

fn find_trace_by_request_id(projection: &Value, request_id: &str) -> Option<Value> {
    if request_id.eq_ignore_ascii_case("latest") {
        return latest_trace_from_projection(projection);
    }

    match projection {
        Value::Object(map) => {
            if map.get("trace_id").and_then(Value::as_str) == Some(request_id) {
                return Some(projection.clone());
            }
            if let Some(Value::Array(traces)) = map.get("traces") {
                return traces
                    .iter()
                    .find(|trace| trace.get("trace_id").and_then(Value::as_str) == Some(request_id))
                    .cloned();
            }
            None
        }
        Value::Array(traces) => traces
            .iter()
            .find(|trace| trace.get("trace_id").and_then(Value::as_str) == Some(request_id))
            .cloned(),
        _ => None,
    }
}

async fn get_inspector_circuits(
    headers: HeaderMap,
    State(state): State<InspectorState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    ensure_internal_access(&headers, &state.auth_policy)?;
    let schematic = state.schematic.lock().unwrap();
    let transition_count = schematic
        .nodes
        .iter()
        .filter(|node| matches!(&node.kind, NodeKind::Atom))
        .count();
    let has_capability_rules = schematic.nodes.iter().any(|node| {
        node.bus_capability
            .as_ref()
            .map(|policy| !policy.allow.is_empty() || !policy.deny.is_empty())
            .unwrap_or(false)
    });

    Ok(inspector_envelope(
        "inspector.circuits.v1",
        serde_json::json!({
            "count": 1,
            "items": [
                {
                    "id": schematic.id,
                    "name": schematic.name,
                    "schema_version": schematic.schema_version,
                    "node_count": schematic.nodes.len(),
                    "edge_count": schematic.edges.len(),
                    "transition_count": transition_count,
                    "has_bus_capability_rules": has_capability_rules
                }
            ]
        }),
    ))
}

async fn get_inspector_circuit_by_name(
    headers: HeaderMap,
    AxPath(name): AxPath<String>,
    State(state): State<InspectorState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    ensure_internal_access(&headers, &state.auth_policy)?;
    let schematic = state.schematic.lock().unwrap().clone();
    if name != schematic.name && name != schematic.id {
        return Err(policy_error(StatusCode::NOT_FOUND, "circuit_not_found"));
    }

    let public_projection_loaded = state
        .public_projection
        .lock()
        .ok()
        .map(|slot| slot.is_some())
        .unwrap_or(false);
    let internal_projection_loaded = state
        .internal_projection
        .lock()
        .ok()
        .map(|slot| slot.is_some())
        .unwrap_or(false);

    Ok(inspector_envelope(
        "inspector.circuit.v1",
        serde_json::json!({
            "circuit": {
                "id": schematic.id,
                "name": schematic.name,
                "schema_version": schematic.schema_version,
                "node_count": schematic.nodes.len(),
                "edge_count": schematic.edges.len(),
            },
            "runtime_state": {
                "public_projection_loaded": public_projection_loaded,
                "internal_projection_loaded": internal_projection_loaded,
                "public_projection_path": state.public_projection_path,
                "internal_projection_path": state.internal_projection_path,
            },
            "schematic": schematic
        }),
    ))
}

async fn get_inspector_bus(
    headers: HeaderMap,
    State(state): State<InspectorState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    ensure_internal_access(&headers, &state.auth_policy)?;
    let schematic = state.schematic.lock().unwrap();

    let mut resource_types = HashSet::new();
    let mut transition_capabilities = Vec::new();

    for node in &schematic.nodes {
        if !node.resource_type.trim().is_empty() && node.resource_type != "()" {
            resource_types.insert(node.resource_type.clone());
        }
        if !matches!(&node.kind, NodeKind::Atom) {
            continue;
        }

        let mut allow = node
            .bus_capability
            .as_ref()
            .map(|policy| policy.allow.clone())
            .unwrap_or_default();
        let mut deny = node
            .bus_capability
            .as_ref()
            .map(|policy| policy.deny.clone())
            .unwrap_or_default();
        allow.sort();
        deny.sort();
        let access = if allow.is_empty() && deny.is_empty() {
            "unrestricted"
        } else {
            "restricted"
        };

        transition_capabilities.push(serde_json::json!({
            "transition": node.label,
            "resource_type": node.resource_type,
            "access": access,
            "allow": allow,
            "deny": deny
        }));
    }

    let mut resources = resource_types.into_iter().collect::<Vec<_>>();
    resources.sort();

    Ok(inspector_envelope(
        "inspector.bus.v1",
        serde_json::json!({
            "resource_types": resources,
            "transition_capabilities": transition_capabilities
        }),
    ))
}

async fn get_inspector_timeline_by_request_id(
    headers: HeaderMap,
    AxPath(request_id): AxPath<String>,
    State(state): State<InspectorState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    ensure_internal_access(&headers, &state.auth_policy)?;
    let projection = load_internal_projection_value(&state);
    let trace = find_trace_by_request_id(&projection, &request_id)
        .ok_or_else(|| policy_error(StatusCode::NOT_FOUND, "timeline_request_not_found"))?;

    Ok(inspector_envelope(
        "inspector.timeline.v1",
        serde_json::json!({
            "request_id": request_id,
            "trace": trace
        }),
    ))
}

async fn ws_handler(
    headers: HeaderMap,
    ws: WebSocketUpgrade,
    State(state): State<InspectorState>,
) -> impl IntoResponse {
    if let Err(err) = ensure_internal_access(&headers, &state.auth_policy) {
        return err.into_response();
    }
    ws.on_upgrade(handle_socket)
}

async fn get_quick_view_html() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        QUICK_VIEW_HTML,
    )
}

async fn get_quick_view_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        QUICK_VIEW_JS,
    )
}

async fn get_quick_view_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        QUICK_VIEW_CSS,
    )
}

async fn handle_socket(mut socket: WebSocket) {
    let mut rx = get_sender().subscribe();

    while let Ok(msg) = rx.recv().await {
        if socket.send(Message::Text(msg)).await.is_err() {
            break;
        }
    }
}

fn env_flag(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(v) => matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "on" | "yes"),
        Err(_) => default,
    }
}

fn parse_role(headers: &HeaderMap) -> Result<AccessRole, &'static str> {
    let raw = headers
        .get("x-ranvier-role")
        .ok_or("missing_x_ranvier_role")?
        .to_str()
        .map_err(|_| "invalid_x_ranvier_role")?
        .trim()
        .to_ascii_lowercase();

    match raw.as_str() {
        "viewer" => Ok(AccessRole::Viewer),
        "operator" => Ok(AccessRole::Operator),
        "admin" => Ok(AccessRole::Admin),
        _ => Err("invalid_x_ranvier_role"),
    }
}

fn has_tenant(headers: &HeaderMap) -> bool {
    headers
        .get("x-ranvier-tenant")
        .and_then(|v| v.to_str().ok())
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
}

fn policy_error(code: StatusCode, message: &'static str) -> (StatusCode, Json<Value>) {
    (
        code,
        Json(serde_json::json!({
            "error": message
        })),
    )
}

fn ensure_public_access(
    headers: &HeaderMap,
    policy: &AuthPolicy,
) -> Result<(), (StatusCode, Json<Value>)> {
    if !policy.enforce_headers {
        return Ok(());
    }
    parse_role(headers)
        .map(|_| ())
        .map_err(|e| policy_error(StatusCode::UNAUTHORIZED, e))
}

fn ensure_internal_access(
    headers: &HeaderMap,
    policy: &AuthPolicy,
) -> Result<(), (StatusCode, Json<Value>)> {
    if !policy.enforce_headers {
        return Ok(());
    }

    let role = parse_role(headers).map_err(|e| policy_error(StatusCode::UNAUTHORIZED, e))?;
    if role == AccessRole::Viewer {
        return Err(policy_error(
            StatusCode::FORBIDDEN,
            "role_forbidden_for_internal_endpoint",
        ));
    }
    if policy.require_tenant_for_internal && !has_tenant(headers) {
        return Err(policy_error(
            StatusCode::FORBIDDEN,
            "missing_x_ranvier_tenant",
        ));
    }
    Ok(())
}

// --- M201: New API endpoints ---

async fn api_get_routes(
    headers: HeaderMap,
    State(state): State<InspectorState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    ensure_internal_access(&headers, &state.auth_policy)?;

    let registered = routes::list_routes();

    // Also enrich from schematic node schemas
    let schematic = state.schematic.lock().unwrap();
    let mut route_list: Vec<Value> = registered
        .iter()
        .map(|r| {
            // Find matching node in schematic by label
            let node_schemas = schematic.nodes.iter().find(|n| {
                Some(r.circuit_name.as_deref().unwrap_or(""))
                    == Some(n.label.as_str())
            });

            serde_json::json!({
                "method": r.method,
                "path": r.path,
                "circuit_name": r.circuit_name,
                "input_schema": r.input_schema.clone().or_else(|| node_schemas.and_then(|n| n.input_schema.clone())),
                "output_schema": r.output_schema.clone().or_else(|| node_schemas.and_then(|n| n.output_schema.clone())),
            })
        })
        .collect();

    // If no routes registered, build from schematic nodes
    if route_list.is_empty() {
        route_list = schematic
            .nodes
            .iter()
            .filter(|n| n.input_schema.is_some() || n.output_schema.is_some())
            .map(|n| {
                serde_json::json!({
                    "circuit_name": n.label,
                    "input_type": n.input_type,
                    "output_type": n.output_type,
                    "input_schema": n.input_schema,
                    "output_schema": n.output_schema,
                })
            })
            .collect();
    }

    Ok(inspector_envelope(
        "inspector.routes.v1",
        serde_json::json!({
            "count": route_list.len(),
            "routes": route_list
        }),
    ))
}

async fn api_post_routes_schema(
    headers: HeaderMap,
    State(state): State<InspectorState>,
    Json(body): Json<routes::SchemaLookupRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    ensure_internal_access(&headers, &state.auth_policy)?;

    // Look up from route registry first
    if let Some(route) = routes::find_route(&body.method, &body.path) {
        if let Some(schema) = route.input_schema {
            return Ok(inspector_envelope(
                "inspector.route_schema.v1",
                serde_json::json!({
                    "method": body.method,
                    "path": body.path,
                    "schema": schema
                }),
            ));
        }
    }

    // Fallback: look in schematic nodes
    let schematic = state.schematic.lock().unwrap();
    for node in &schematic.nodes {
        if let Some(schema) = &node.input_schema {
            return Ok(inspector_envelope(
                "inspector.route_schema.v1",
                serde_json::json!({
                    "method": body.method,
                    "path": body.path,
                    "schema": schema
                }),
            ));
        }
    }

    Err(policy_error(StatusCode::NOT_FOUND, "schema_not_found"))
}

async fn api_post_routes_sample(
    headers: HeaderMap,
    State(state): State<InspectorState>,
    Json(body): Json<routes::SampleRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    ensure_internal_access(&headers, &state.auth_policy)?;

    // Find the schema for this route
    let schema_val = {
        if let Some(route) = routes::find_route(&body.method, &body.path) {
            route.input_schema
        } else {
            // Fallback: look in schematic nodes
            let schematic = state.schematic.lock().unwrap();
            schematic
                .nodes
                .iter()
                .find_map(|n| n.input_schema.clone())
        }
    };

    let Some(schema_val) = schema_val else {
        return Err(policy_error(StatusCode::NOT_FOUND, "schema_not_found"));
    };

    let sample = match body.mode.as_str() {
        "random" => schema::generate_sample(&schema_val),
        _ => schema::generate_template(&schema_val),
    };

    Ok(inspector_envelope(
        "inspector.route_sample.v1",
        serde_json::json!({
            "method": body.method,
            "path": body.path,
            "mode": body.mode,
            "sample": sample
        }),
    ))
}

async fn api_post_relay(
    headers: HeaderMap,
    State(state): State<InspectorState>,
    Json(body): Json<relay::RelayRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    ensure_internal_access(&headers, &state.auth_policy)?;

    // Guard: dev mode only
    let mode = InspectorMode::from_env();
    if mode != InspectorMode::Dev {
        return Err(policy_error(
            StatusCode::FORBIDDEN,
            "relay_only_in_dev_mode",
        ));
    }

    let Some(relay_state) = &state.relay_state else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "relay_not_configured",
                "message": "No relay target configured. Use Inspector::with_relay_target() to enable."
            })),
        ));
    };

    match relay::execute_relay(relay_state, body).await {
        Ok(response) => Ok(inspector_envelope(
            "inspector.relay.v1",
            serde_json::to_value(&response).unwrap_or(Value::Null),
        )),
        Err(err) => Err((
            StatusCode::BAD_GATEWAY,
            Json(serde_json::to_value(&err).unwrap_or(Value::Null)),
        )),
    }
}

async fn api_get_stored_traces(
    headers: HeaderMap,
    State(state): State<InspectorState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    ensure_internal_access(&headers, &state.auth_policy)?;
    state.bearer_auth.validate(&headers)?;

    let Some(store) = &state.trace_store else {
        return Ok(inspector_envelope(
            "inspector.traces_stored.v1",
            serde_json::json!({
                "total": 0,
                "traces": [],
                "note": "No trace store configured"
            }),
        ));
    };

    let query = trace_store::TraceQuery::default();
    match store.query(query).await {
        Ok(traces) => Ok(inspector_envelope(
            "inspector.traces_stored.v1",
            serde_json::json!({
                "total": traces.len(),
                "traces": traces
            }),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "trace_store_error", "message": e })),
        )),
    }
}

async fn api_get_lineage(
    headers: HeaderMap,
    AxPath(trace_id): AxPath<String>,
    State(state): State<InspectorState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    ensure_internal_access(&headers, &state.auth_policy)?;
    state.bearer_auth.validate(&headers)?;

    let Some(store) = &state.trace_store else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "no_trace_store" })),
        ));
    };

    let trace = store.get(&trace_id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "trace_store_error", "message": e })),
        )
    })?;

    let Some(trace) = trace else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "trace_not_found", "trace_id": trace_id })),
        ));
    };

    match lineage::extract_lineage(&trace) {
        Some(lin) => Ok(inspector_envelope(
            "inspector.lineage.v1",
            serde_json::to_value(&lin).unwrap_or_default(),
        )),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "no_lineage_data",
                "trace_id": trace_id,
                "note": "Trace has no timeline_json"
            })),
        )),
    }
}

#[derive(Deserialize)]
struct TraceDiffQuery {
    a: String,
    b: String,
}

async fn api_get_trace_diff(
    headers: HeaderMap,
    Query(params): Query<TraceDiffQuery>,
    State(state): State<InspectorState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    ensure_internal_access(&headers, &state.auth_policy)?;
    state.bearer_auth.validate(&headers)?;

    let Some(store) = &state.trace_store else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "no_trace_store" })),
        ));
    };

    let trace_a = store.get(&params.a).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "trace_store_error", "message": e })),
        )
    })?;
    let trace_b = store.get(&params.b).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "trace_store_error", "message": e })),
        )
    })?;

    let Some(trace_a) = trace_a else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "trace_not_found", "trace_id": params.a })),
        ));
    };
    let Some(trace_b) = trace_b else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "trace_not_found", "trace_id": params.b })),
        ));
    };

    let lin_a = lineage::extract_lineage(&trace_a).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "no_lineage_data", "trace_id": params.a })),
        )
    })?;
    let lin_b = lineage::extract_lineage(&trace_b).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "no_lineage_data", "trace_id": params.b })),
        )
    })?;

    let diff = lineage::diff_traces(&lin_a, &lin_b);
    Ok(inspector_envelope(
        "inspector.trace_diff.v1",
        serde_json::to_value(&diff).unwrap_or_default(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ranvier_core::schematic::Schematic;
    use std::time::Duration;

    fn reserve_listener() -> (u16, tokio::net::TcpListener) {
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        std_listener.set_nonblocking(true).expect("set nonblocking");
        let port = std_listener.local_addr().expect("local addr").port();
        let listener =
            tokio::net::TcpListener::from_std(std_listener).expect("tokio listener conversion");
        (port, listener)
    }

    async fn wait_ready(port: u16) {
        let client = reqwest::Client::new();
        for _ in 0..30 {
            if client
                .get(format!("http://127.0.0.1:{port}/schematic"))
                .send()
                .await
                .map(|_| true)
                .unwrap_or(false)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("inspector server did not become ready");
    }

    #[test]
    fn redaction_defaults_public_and_internal_surfaces() {
        let policy = TelemetryRedactionPolicy::default();
        let src = serde_json::json!({
            "email": "user@example.com",
            "summary": { "ok": true }
        });

        let public = apply_projection_redaction(src.clone(), ProjectionSurface::Public, &policy);
        let internal = apply_projection_redaction(src, ProjectionSurface::Internal, &policy);

        assert_eq!(public["email"], "[REDACTED]");
        assert_eq!(public["summary"]["ok"], true);
        assert_eq!(internal["email"], "user@example.com");
    }

    #[test]
    fn strict_mode_filters_attribute_bag_by_allowlist() {
        let policy = TelemetryRedactionPolicy {
            mode_override: Some(RedactionMode::Strict),
            allow_keys: {
                let mut keys = std::collections::HashSet::new();
                keys.insert("ranvier.circuit".to_string());
                keys
            },
            ..Default::default()
        };

        let src = serde_json::json!({
            "attributes": {
                "ranvier.circuit": "CheckoutCircuit",
                "customer_id": "u-123",
                "api_key": "secret-key"
            }
        });

        let out = apply_projection_redaction(src, ProjectionSurface::Internal, &policy);
        assert_eq!(out["attributes"]["ranvier.circuit"], "CheckoutCircuit");
        assert_eq!(out["attributes"]["api_key"], "[REDACTED]");
        assert!(out["attributes"].get("customer_id").is_none());
    }

    #[test]
    fn custom_sensitive_patterns_are_applied() {
        let mut policy = TelemetryRedactionPolicy::default();
        policy.sensitive_patterns.push("tenant_id".to_string());

        let src = serde_json::json!({
            "tenant_id": "team-a",
            "trace_id": "abc123"
        });

        let out = apply_projection_redaction(src, ProjectionSurface::Public, &policy);
        assert_eq!(out["tenant_id"], "[REDACTED]");
        assert_eq!(out["trace_id"], "abc123");
    }

    #[tokio::test]
    async fn dev_mode_exposes_quick_view_and_internal_routes() {
        let (port, listener) = reserve_listener();
        let inspector = Inspector::new(Schematic::new("dev-test"), port).with_mode("dev");
        let handle = tokio::spawn(async move {
            let _ = inspector.serve_with_listener(listener).await;
        });
        wait_ready(port).await;

        let client = reqwest::Client::new();
        let quick = client
            .get(format!("http://127.0.0.1:{port}/quick-view"))
            .send()
            .await
            .expect("quick-view request");
        let internal = client
            .get(format!("http://127.0.0.1:{port}/trace/internal"))
            .send()
            .await
            .expect("internal request");
        let events = client
            .get(format!("http://127.0.0.1:{port}/events"))
            .send()
            .await
            .expect("events request");
        let circuits = client
            .get(format!("http://127.0.0.1:{port}/inspector/circuits"))
            .send()
            .await
            .expect("circuits request");
        let bus = client
            .get(format!("http://127.0.0.1:{port}/inspector/bus"))
            .send()
            .await
            .expect("bus request");
        let timeline = client
            .get(format!(
                "http://127.0.0.1:{port}/inspector/timeline/bootstrap"
            ))
            .send()
            .await
            .expect("timeline request");

        assert_eq!(quick.status(), reqwest::StatusCode::OK);
        assert_eq!(internal.status(), reqwest::StatusCode::OK);
        assert_ne!(events.status(), reqwest::StatusCode::NOT_FOUND);
        assert_eq!(circuits.status(), reqwest::StatusCode::OK);
        assert_eq!(bus.status(), reqwest::StatusCode::OK);
        assert_eq!(timeline.status(), reqwest::StatusCode::OK);
        let circuits_json: Value =
            serde_json::from_str(&circuits.text().await.expect("circuits text"))
                .expect("circuits json");
        let bus_json: Value =
            serde_json::from_str(&bus.text().await.expect("bus text")).expect("bus json");
        let timeline_json: Value =
            serde_json::from_str(&timeline.text().await.expect("timeline text"))
                .expect("timeline json");
        assert_eq!(circuits_json["kind"], "inspector.circuits.v1");
        assert_eq!(bus_json["kind"], "inspector.bus.v1");
        assert_eq!(timeline_json["kind"], "inspector.timeline.v1");

        handle.abort();
    }

    #[tokio::test]
    async fn prod_mode_hides_quick_view_and_internal_routes() {
        let (port, listener) = reserve_listener();
        let inspector = Inspector::new(Schematic::new("prod-test"), port).with_mode("prod");
        let handle = tokio::spawn(async move {
            let _ = inspector.serve_with_listener(listener).await;
        });
        wait_ready(port).await;

        let client = reqwest::Client::new();
        let quick = client
            .get(format!("http://127.0.0.1:{port}/quick-view"))
            .send()
            .await
            .expect("quick-view request");
        let internal = client
            .get(format!("http://127.0.0.1:{port}/trace/internal"))
            .send()
            .await
            .expect("internal request");
        let events = client
            .get(format!("http://127.0.0.1:{port}/events"))
            .send()
            .await
            .expect("events request");
        let circuits = client
            .get(format!("http://127.0.0.1:{port}/inspector/circuits"))
            .send()
            .await
            .expect("circuits request");
        let public = client
            .get(format!("http://127.0.0.1:{port}/trace/public"))
            .send()
            .await
            .expect("public request");

        assert_eq!(quick.status(), reqwest::StatusCode::NOT_FOUND);
        assert_eq!(internal.status(), reqwest::StatusCode::NOT_FOUND);
        assert_eq!(events.status(), reqwest::StatusCode::NOT_FOUND);
        assert_eq!(circuits.status(), reqwest::StatusCode::NOT_FOUND);
        assert_eq!(public.status(), reqwest::StatusCode::OK);

        handle.abort();
    }

    #[tokio::test]
    async fn timeline_endpoint_returns_not_found_for_unknown_request() {
        let (port, listener) = reserve_listener();
        let inspector = Inspector::new(Schematic::new("timeline-test"), port).with_mode("dev");
        let handle = tokio::spawn(async move {
            let _ = inspector.serve_with_listener(listener).await;
        });
        wait_ready(port).await;

        let client = reqwest::Client::new();
        let timeline = client
            .get(format!(
                "http://127.0.0.1:{port}/inspector/timeline/unknown-request"
            ))
            .send()
            .await
            .expect("timeline request");
        assert_eq!(timeline.status(), reqwest::StatusCode::NOT_FOUND);

        handle.abort();
    }

    #[tokio::test]
    async fn auth_enforcement_rejects_missing_role_header() {
        let (port, listener) = reserve_listener();
        let inspector = Inspector::new(Schematic::new("auth-public"), port)
            .with_mode("dev")
            .with_auth_enforcement(true);
        let handle = tokio::spawn(async move {
            let _ = inspector.serve_with_listener(listener).await;
        });
        wait_ready(port).await;

        let client = reqwest::Client::new();
        let schematic = client
            .get(format!("http://127.0.0.1:{port}/schematic"))
            .send()
            .await
            .expect("schematic request");
        assert_eq!(schematic.status(), reqwest::StatusCode::UNAUTHORIZED);

        handle.abort();
    }

    #[tokio::test]
    async fn auth_enforcement_blocks_viewer_internal_and_requires_tenant() {
        let (port, listener) = reserve_listener();
        let inspector = Inspector::new(Schematic::new("auth-internal"), port)
            .with_mode("dev")
            .with_auth_enforcement(true)
            .with_require_tenant_for_internal(true);
        let handle = tokio::spawn(async move {
            let _ = inspector.serve_with_listener(listener).await;
        });
        wait_ready(port).await;

        let client = reqwest::Client::new();

        let viewer_internal = client
            .get(format!("http://127.0.0.1:{port}/trace/internal"))
            .header("X-Ranvier-Role", "viewer")
            .send()
            .await
            .expect("viewer internal request");
        assert_eq!(viewer_internal.status(), reqwest::StatusCode::FORBIDDEN);

        let operator_no_tenant = client
            .get(format!("http://127.0.0.1:{port}/trace/internal"))
            .header("X-Ranvier-Role", "operator")
            .send()
            .await
            .expect("operator internal request");
        assert_eq!(operator_no_tenant.status(), reqwest::StatusCode::FORBIDDEN);

        let operator_with_tenant = client
            .get(format!("http://127.0.0.1:{port}/trace/internal"))
            .header("X-Ranvier-Role", "operator")
            .header("X-Ranvier-Tenant", "team-a")
            .send()
            .await
            .expect("operator internal with tenant request");
        assert_eq!(operator_with_tenant.status(), reqwest::StatusCode::OK);

        handle.abort();
    }

    #[test]
    fn active_trace_registry_ring_buffer_evicts_oldest() {
        let mut registry = ActiveTraceRegistry::with_config(3, 0);

        for i in 0..5 {
            let record = TraceRecord {
                trace_id: format!("t-{i}"),
                circuit: "Test".to_string(),
                status: TraceStatus::Completed,
                started_at: 1000 + i * 100,
                finished_at: Some(1100 + i * 100),
                duration_ms: Some(100),
                outcome_type: Some("Next".to_string()),
            };
            registry.recent.push_back(record);
            while registry.recent.len() > registry.max_recent {
                registry.recent.pop_front();
            }
        }

        assert_eq!(registry.recent_count(), 3);
        assert_eq!(registry.recent.front().unwrap().trace_id, "t-2");
        assert_eq!(registry.recent.back().unwrap().trace_id, "t-4");
    }

    #[test]
    fn active_trace_registry_ttl_prunes_expired() {
        let now = epoch_ms();
        let mut registry = ActiveTraceRegistry::with_config(100, 1000); // 1 second TTL

        // Insert a trace from 2 seconds ago
        registry.recent.push_back(TraceRecord {
            trace_id: "old".to_string(),
            circuit: "Test".to_string(),
            status: TraceStatus::Completed,
            started_at: now.saturating_sub(2000),
            finished_at: Some(now.saturating_sub(1900)),
            duration_ms: Some(100),
            outcome_type: Some("Next".to_string()),
        });

        // Insert a fresh trace
        registry.recent.push_back(TraceRecord {
            trace_id: "fresh".to_string(),
            circuit: "Test".to_string(),
            status: TraceStatus::Completed,
            started_at: now,
            finished_at: Some(now + 100),
            duration_ms: Some(100),
            outcome_type: Some("Next".to_string()),
        });

        // Prune should remove the old trace
        registry.prune_expired();
        assert_eq!(registry.recent_count(), 1);
        assert_eq!(registry.recent.front().unwrap().trace_id, "fresh");
    }

    #[test]
    fn trace_registry_config_defaults() {
        let config = TraceRegistryConfig::default();
        assert_eq!(config.max_traces, 10_000);
        assert_eq!(config.trace_ttl, std::time::Duration::from_secs(3600));
    }

    #[tokio::test]
    async fn allow_unauthenticated_suppresses_auth_warning() {
        let (port, listener) = reserve_listener();
        // This should start without auth issues and without warnings in debug mode
        let inspector = Inspector::new(Schematic::new("unauth-test"), port)
            .with_mode("dev")
            .allow_unauthenticated();
        let handle = tokio::spawn(async move {
            let _ = inspector.serve_with_listener(listener).await;
        });
        wait_ready(port).await;

        let client = reqwest::Client::new();
        let schematic = client
            .get(format!("http://127.0.0.1:{port}/schematic"))
            .send()
            .await
            .expect("schematic request");
        assert_eq!(schematic.status(), reqwest::StatusCode::OK);

        handle.abort();
    }

    #[tokio::test]
    async fn bearer_auth_protects_metrics_endpoint() {
        let (port, listener) = reserve_listener();
        let inspector = Inspector::new(Schematic::new("bearer-test"), port)
            .with_mode("dev")
            .with_bearer_token("secret-token-123");
        let handle = tokio::spawn(async move {
            let _ = inspector.serve_with_listener(listener).await;
        });
        wait_ready(port).await;

        let client = reqwest::Client::new();

        // Without token → 401
        let no_auth = client
            .get(format!("http://127.0.0.1:{port}/metrics"))
            .send()
            .await
            .expect("metrics without auth");
        assert_eq!(no_auth.status(), reqwest::StatusCode::UNAUTHORIZED);

        // With wrong token → 401
        let wrong_auth = client
            .get(format!("http://127.0.0.1:{port}/metrics"))
            .header("Authorization", "Bearer wrong-token")
            .send()
            .await
            .expect("metrics with wrong token");
        assert_eq!(wrong_auth.status(), reqwest::StatusCode::UNAUTHORIZED);

        // With correct token → 200
        let correct_auth = client
            .get(format!("http://127.0.0.1:{port}/metrics"))
            .header("Authorization", "Bearer secret-token-123")
            .send()
            .await
            .expect("metrics with correct token");
        assert_eq!(correct_auth.status(), reqwest::StatusCode::OK);

        handle.abort();
    }
}
