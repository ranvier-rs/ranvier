use axum::{
    Json, Router,
    extract::{
        Path as AxPath, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    routing::get,
};
use ranvier_core::schematic::{NodeKind, Schematic};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::broadcast;
use tower_http::cors::CorsLayer;
use tracing::{Event, Id, Subscriber};
use tracing_subscriber::{Layer, layer::Context, registry::LookupSpan};

static EVENT_CHANNEL: OnceLock<broadcast::Sender<String>> = OnceLock::new();
const QUICK_VIEW_HTML: &str = include_str!("quick_view/index.html");
const QUICK_VIEW_JS: &str = include_str!("quick_view/app.js");
const QUICK_VIEW_CSS: &str = include_str!("quick_view/styles.css");
const INSPECTOR_API_VERSION: &str = "1.0";

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
        }
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

    pub async fn serve(self) -> Result<(), std::io::Error> {
        let addr = SocketAddr::from(([0, 0, 0, 0], self.port));
        let listener = tokio::net::TcpListener::bind(addr).await?;
        self.serve_with_listener(listener).await
    }

    pub async fn serve_with_listener(
        self,
        listener: tokio::net::TcpListener,
    ) -> Result<(), std::io::Error> {
        let state = InspectorState {
            schematic: self.schematic.clone(),
            public_projection: self.public_projection.clone(),
            internal_projection: self.internal_projection.clone(),
            public_projection_path: self.public_projection_path.clone(),
            internal_projection_path: self.internal_projection_path.clone(),
            auth_policy: self.auth_policy,
            redaction_policy: self.redaction_policy.clone(),
        };

        let mut app = Router::new()
            .route("/schematic", get(get_schematic))
            .route("/trace/public", get(get_public_projection))
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
                );
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

#[derive(Clone)]
struct InspectorState {
    schematic: Arc<Mutex<Schematic>>,
    public_projection: Arc<Mutex<Option<Value>>>,
    internal_projection: Arc<Mutex<Option<Value>>>,
    public_projection_path: Option<String>,
    internal_projection_path: Option<String>,
    auth_policy: AuthPolicy,
    redaction_policy: TelemetryRedactionPolicy,
}

pub fn layer() -> InspectorLayer {
    InspectorLayer
}

pub struct InspectorLayer;

impl<S> Layer<S> for InspectorLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        if metadata.target().starts_with("ranvier") {
            // Simple JSON serialization of the event
            // In a real impl, we'd use a visitor to extract fields
            let msg = format!(
                "{{\"type\": \"event\", \"target\": \"{}\", \"level\": \"{}\"}}",
                metadata.target(),
                metadata.level()
            );
            let _ = get_sender().send(msg);
        }
    }

    // Using on_enter/exit for Span tracking would be better for Node visualization
    fn on_enter(&self, id: &Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            if span.name() == "Node" {
                // Send Node Enter Event
                // We need extensions to really get data, but name is a start
                let msg = format!(
                    "{{\"type\": \"node_enter\", \"name\": \"{}\"}}",
                    span.name()
                );
                let _ = get_sender().send(msg);
            }
        }
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
    if let Some(path) = &state.public_projection_path {
        if let Ok(v) = read_projection_file(path) {
            return Ok(Json(apply_projection_redaction(
                v,
                ProjectionSurface::Public,
                &state.redaction_policy,
            )));
        }
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
    if let Some(path) = &state.internal_projection_path {
        if let Ok(v) = read_projection_file(path) {
            return Ok(Json(apply_projection_redaction(
                v,
                ProjectionSurface::Internal,
                &state.redaction_policy,
            )));
        }
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
    if let Some(path) = &state.internal_projection_path {
        if let Ok(v) = read_projection_file(path) {
            return v;
        }
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
        let mut policy = TelemetryRedactionPolicy::default();
        policy.mode_override = Some(RedactionMode::Strict);
        policy.allow_keys.insert("ranvier.circuit".to_string());

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
}
