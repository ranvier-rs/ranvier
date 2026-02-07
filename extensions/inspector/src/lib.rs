use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use ranvier_core::schematic::{NodeKind, Schematic};
use serde_json::Value;
use std::fs;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::broadcast;
use tower_http::cors::CorsLayer;
use tracing::{Event, Id, Subscriber};
use tracing_subscriber::{layer::Context, registry::LookupSpan, Layer};

static EVENT_CHANNEL: OnceLock<broadcast::Sender<String>> = OnceLock::new();
const QUICK_VIEW_HTML: &str = include_str!("quick_view/index.html");
const QUICK_VIEW_JS: &str = include_str!("quick_view/app.js");
const QUICK_VIEW_CSS: &str = include_str!("quick_view/styles.css");

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

    pub async fn serve(self) -> Result<(), std::io::Error> {
        let state = InspectorState {
            schematic: self.schematic.clone(),
            public_projection: self.public_projection.clone(),
            internal_projection: self.internal_projection.clone(),
            public_projection_path: self.public_projection_path.clone(),
            internal_projection_path: self.internal_projection_path.clone(),
            auth_policy: self.auth_policy,
        };

        let mut app = Router::new()
            .route("/schematic", get(get_schematic))
            .route("/trace/public", get(get_public_projection))
            .layer(CorsLayer::permissive());

        if self.surface_policy.expose_internal {
            app = app.route("/trace/internal", get(get_internal_projection));
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

        let addr = SocketAddr::from(([0, 0, 0, 0], self.port));
        tracing::info!("Ranvier Inspector listening on http://{}", addr);

        let listener = tokio::net::TcpListener::bind(addr).await?;
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

#[derive(Clone)]
struct InspectorState {
    schematic: Arc<Mutex<Schematic>>,
    public_projection: Arc<Mutex<Option<Value>>>,
    internal_projection: Arc<Mutex<Option<Value>>>,
    public_projection_path: Option<String>,
    internal_projection_path: Option<String>,
    auth_policy: AuthPolicy,
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
            return Ok(Json(v));
        }
    }

    let projection = state
        .public_projection
        .lock()
        .ok()
        .and_then(|v| v.clone())
        .unwrap_or(Value::Null);
    Ok(Json(projection))
}

async fn get_internal_projection(
    headers: HeaderMap,
    State(state): State<InspectorState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    ensure_internal_access(&headers, &state.auth_policy)?;
    if let Some(path) = &state.internal_projection_path {
        if let Ok(v) = read_projection_file(path) {
            return Ok(Json(v));
        }
    }

    let projection = state
        .internal_projection
        .lock()
        .ok()
        .and_then(|v| v.clone())
        .unwrap_or(Value::Null);
    Ok(Json(projection))
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
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], QUICK_VIEW_HTML)
}

async fn get_quick_view_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/javascript; charset=utf-8")],
        QUICK_VIEW_JS,
    )
}

async fn get_quick_view_css() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], QUICK_VIEW_CSS)
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

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .expect("bind ephemeral port")
            .local_addr()
            .expect("local addr")
            .port()
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

    #[tokio::test]
    async fn dev_mode_exposes_quick_view_and_internal_routes() {
        let port = free_port();
        let inspector = Inspector::new(Schematic::new("dev-test"), port).with_mode("dev");
        let handle = tokio::spawn(async move {
            let _ = inspector.serve().await;
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

        assert_eq!(quick.status(), reqwest::StatusCode::OK);
        assert_eq!(internal.status(), reqwest::StatusCode::OK);
        assert_ne!(events.status(), reqwest::StatusCode::NOT_FOUND);

        handle.abort();
    }

    #[tokio::test]
    async fn prod_mode_hides_quick_view_and_internal_routes() {
        let port = free_port();
        let inspector = Inspector::new(Schematic::new("prod-test"), port).with_mode("prod");
        let handle = tokio::spawn(async move {
            let _ = inspector.serve().await;
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
        let public = client
            .get(format!("http://127.0.0.1:{port}/trace/public"))
            .send()
            .await
            .expect("public request");

        assert_eq!(quick.status(), reqwest::StatusCode::NOT_FOUND);
        assert_eq!(internal.status(), reqwest::StatusCode::NOT_FOUND);
        assert_eq!(events.status(), reqwest::StatusCode::NOT_FOUND);
        assert_eq!(public.status(), reqwest::StatusCode::OK);

        handle.abort();
    }

    #[tokio::test]
    async fn auth_enforcement_rejects_missing_role_header() {
        let port = free_port();
        let inspector = Inspector::new(Schematic::new("auth-public"), port)
            .with_mode("dev")
            .with_auth_enforcement(true);
        let handle = tokio::spawn(async move {
            let _ = inspector.serve().await;
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
        let port = free_port();
        let inspector = Inspector::new(Schematic::new("auth-internal"), port)
            .with_mode("dev")
            .with_auth_enforcement(true)
            .with_require_tenant_for_internal(true);
        let handle = tokio::spawn(async move {
            let _ = inspector.serve().await;
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
