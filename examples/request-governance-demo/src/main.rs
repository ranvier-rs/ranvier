//! Request Governance Demo
//!
//! ## Purpose
//! Demonstrates explicit request governance wiring: auth, policy, audit,
//! persistence, and structured error responses.
//!
//! ## Run
//! ```bash
//! cargo run -p request-governance-demo
//! ```
//!
//! ## Key Concepts
//! - JWT login with role claims
//! - Explicit role checks in transitions
//! - SQLite persistence for requests and audit events
//! - RFC 7807 error mapping for governance routes
//! - Access-log guard as request-level observability touchpoint
//!
//! ## Prerequisites
//! - `admin-crud-demo` — bridge backend example
//! - `reference-fullstack-admin` — public-only reference app
//!
//! ## Next Steps
//! - `reference-ecommerce-order` — workflow-heavy reference app

use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use ranvier_core::prelude::*;
use ranvier_core::transition::ResourceRequirement;
use ranvier_guard::prelude::*;
use ranvier_http::prelude::*;
use ranvier_runtime::Axon;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool, sqlite::SqlitePoolOptions};
use thiserror::Error;

static JWT_SECRET: LazyLock<String> = LazyLock::new(|| {
    std::env::var("JWT_SECRET").unwrap_or_else(|_| "request-governance-demo-secret".to_string())
});

#[derive(Clone)]
struct RequestHeaders(HashMap<String, String>);

#[derive(Clone)]
struct AppState {
    pool: SqlitePool,
}

impl ResourceRequirement for AppState {}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Claims {
    sub: String,
    roles: Vec<String>,
    exp: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct LoginRequest {
    username: String,
    password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct LoginResponse {
    token: String,
    username: String,
    roles: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct CreateRequestInput {
    title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct RequestRecord {
    id: i64,
    title: String,
    status: String,
    created_by: String,
    approved_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct ApprovalResult {
    approved: bool,
    request: RequestRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize, Error)]
enum GovernanceError {
    #[error("Unauthorized")]
    Unauthorized,
    #[error("Forbidden")]
    Forbidden,
    #[error("Request not found: {0}")]
    NotFound(i64),
    #[error("Validation error: {0}")]
    Validation(String),
    #[error("Internal error: {0}")]
    Internal(String),
}

impl IntoProblemDetail for GovernanceError {
    fn into_problem_detail(&self) -> ProblemDetail {
        match self {
            GovernanceError::Unauthorized => ProblemDetail::new(401, "Unauthorized"),
            GovernanceError::Forbidden => ProblemDetail::new(403, "Forbidden"),
            GovernanceError::NotFound(id) => ProblemDetail::new(404, "Request Not Found")
                .with_detail(format!("Request {id} does not exist")),
            GovernanceError::Validation(msg) => {
                ProblemDetail::new(400, "Validation Error").with_detail(msg.clone())
            }
            GovernanceError::Internal(msg) => {
                ProblemDetail::new(500, "Internal Server Error").with_detail(msg.clone())
            }
        }
    }
}

fn governance_error_response(error: &GovernanceError) -> HttpResponse {
    error.into_problem_detail().into_response()
}

fn issue_token(username: &str, roles: Vec<String>) -> Result<String, GovernanceError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| GovernanceError::Internal(format!("clock error: {e}")))?
        .as_secs() as usize;

    let claims = Claims {
        sub: username.to_string(),
        roles,
        exp: now + 24 * 60 * 60,
    };

    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(JWT_SECRET.as_bytes()),
    )
    .map_err(|e| GovernanceError::Internal(format!("token encode failed: {e}")))
}

fn verify_token(token: &str) -> Result<Claims, GovernanceError> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_exp = true;

    decode::<Claims>(
        token,
        &DecodingKey::from_secret(JWT_SECRET.as_bytes()),
        &validation,
    )
    .map(|data| data.claims)
    .map_err(|_| GovernanceError::Unauthorized)
}

fn bearer_from_headers(headers: &RequestHeaders) -> Option<&str> {
    headers
        .0
        .get("authorization")
        .and_then(|value| value.strip_prefix("Bearer "))
}

fn require_identity(bus: &mut Bus) -> Result<Claims, GovernanceError> {
    let headers = bus
        .get_cloned::<RequestHeaders>()
        .map_err(|_| GovernanceError::Unauthorized)?;
    let token = bearer_from_headers(&headers).ok_or(GovernanceError::Unauthorized)?;
    verify_token(token)
}

fn require_role(bus: &mut Bus, role: &str) -> Result<Claims, GovernanceError> {
    let claims = require_identity(bus)?;
    if claims.roles.iter().any(|existing| existing == role) {
        Ok(claims)
    } else {
        Err(GovernanceError::Forbidden)
    }
}

async fn initialize_db(pool: &SqlitePool) -> Result<(), GovernanceError> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS requests (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL,
            status TEXT NOT NULL,
            created_by TEXT NOT NULL,
            approved_by TEXT
        )
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| GovernanceError::Internal(format!("create requests failed: {e}")))?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS audit_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            action TEXT NOT NULL,
            request_id INTEGER,
            actor TEXT NOT NULL,
            detail TEXT NOT NULL
        )
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| GovernanceError::Internal(format!("create audit_events failed: {e}")))?;

    Ok(())
}

async fn insert_audit_event(
    pool: &SqlitePool,
    action: &str,
    request_id: Option<i64>,
    actor: &str,
    detail: &str,
) -> Result<(), GovernanceError> {
    sqlx::query(
        r#"
        INSERT INTO audit_events (action, request_id, actor, detail)
        VALUES (?, ?, ?, ?)
        "#,
    )
    .bind(action)
    .bind(request_id)
    .bind(actor)
    .bind(detail)
    .execute(pool)
    .await
    .map_err(|e| GovernanceError::Internal(format!("insert audit event failed: {e}")))?;

    tracing::info!(action, actor, request_id, detail, "audit event inserted");
    Ok(())
}

async fn fetch_request(
    pool: &SqlitePool,
    id: i64,
) -> Result<Option<RequestRecord>, GovernanceError> {
    let row = sqlx::query(
        r#"
        SELECT id, title, status, created_by, approved_by
        FROM requests
        WHERE id = ?
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|e| GovernanceError::Internal(format!("fetch request failed: {e}")))?;

    match row {
        Some(row) => Ok(Some(RequestRecord {
            id: row
                .try_get("id")
                .map_err(|e| GovernanceError::Internal(format!("decode id failed: {e}")))?,
            title: row
                .try_get("title")
                .map_err(|e| GovernanceError::Internal(format!("decode title failed: {e}")))?,
            status: row
                .try_get("status")
                .map_err(|e| GovernanceError::Internal(format!("decode status failed: {e}")))?,
            created_by: row
                .try_get("created_by")
                .map_err(|e| GovernanceError::Internal(format!("decode created_by failed: {e}")))?,
            approved_by: row.try_get("approved_by").map_err(|e| {
                GovernanceError::Internal(format!("decode approved_by failed: {e}"))
            })?,
        })),
        None => Ok(None),
    }
}

#[derive(Clone, Copy)]
struct Login;

#[async_trait]
impl Transition<LoginRequest, LoginResponse> for Login {
    type Error = GovernanceError;
    type Resources = AppState;

    async fn run(
        &self,
        input: LoginRequest,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<LoginResponse, Self::Error> {
        let roles = match (input.username.as_str(), input.password.as_str()) {
            ("admin", "admin123") => vec!["admin".to_string(), "approver".to_string()],
            ("alice", "alice123") => vec!["user".to_string()],
            _ => return Outcome::Fault(GovernanceError::Unauthorized),
        };

        match issue_token(&input.username, roles.clone()) {
            Ok(token) => Outcome::Next(LoginResponse {
                token,
                username: input.username,
                roles,
            }),
            Err(error) => Outcome::Fault(error),
        }
    }
}

#[derive(Clone, Copy)]
struct CreateRequest;

#[async_trait]
impl Transition<CreateRequestInput, RequestRecord> for CreateRequest {
    type Error = GovernanceError;
    type Resources = AppState;

    async fn run(
        &self,
        input: CreateRequestInput,
        resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<RequestRecord, Self::Error> {
        let claims = match require_identity(bus) {
            Ok(claims) => claims,
            Err(error) => return Outcome::Fault(error),
        };

        if input.title.trim().is_empty() {
            return Outcome::Fault(GovernanceError::Validation(
                "title cannot be empty".to_string(),
            ));
        }

        let result = sqlx::query(
            r#"
            INSERT INTO requests (title, status, created_by, approved_by)
            VALUES (?, ?, ?, ?)
            "#,
        )
        .bind(&input.title)
        .bind("pending")
        .bind(&claims.sub)
        .bind(Option::<String>::None)
        .execute(&resources.pool)
        .await;

        let result = match result {
            Ok(result) => result,
            Err(error) => {
                return Outcome::Fault(GovernanceError::Internal(format!(
                    "create request failed: {error}"
                )));
            }
        };

        let id = result.last_insert_rowid();
        if let Err(error) = insert_audit_event(
            &resources.pool,
            "request.created",
            Some(id),
            &claims.sub,
            &input.title,
        )
        .await
        {
            return Outcome::Fault(error);
        }

        match fetch_request(&resources.pool, id).await {
            Ok(Some(record)) => Outcome::Next(record),
            Ok(None) => Outcome::Fault(GovernanceError::Internal(format!(
                "created request missing: {id}"
            ))),
            Err(error) => Outcome::Fault(error),
        }
    }
}

#[derive(Clone, Copy)]
struct GetRequest;

#[async_trait]
impl Transition<(), serde_json::Value> for GetRequest {
    type Error = GovernanceError;
    type Resources = AppState;

    async fn run(
        &self,
        _input: (),
        resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<serde_json::Value, Self::Error> {
        if let Err(error) = require_identity(bus) {
            return Outcome::Fault(error);
        }

        let id: i64 = match bus.path_param("id") {
            Ok(id) => id,
            Err(error) => return Outcome::Fault(GovernanceError::Validation(error)),
        };

        match fetch_request(&resources.pool, id).await {
            Ok(Some(record)) => Outcome::Next(
                serde_json::to_value(record).unwrap_or_else(|_| serde_json::json!({})),
            ),
            Ok(None) => Outcome::Fault(GovernanceError::NotFound(id)),
            Err(error) => Outcome::Fault(error),
        }
    }
}

#[derive(Clone, Copy)]
struct ApproveRequest;

#[async_trait]
impl Transition<(), serde_json::Value> for ApproveRequest {
    type Error = GovernanceError;
    type Resources = AppState;

    async fn run(
        &self,
        _input: (),
        resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<serde_json::Value, Self::Error> {
        let claims = match require_role(bus, "approver") {
            Ok(claims) => claims,
            Err(error) => return Outcome::Fault(error),
        };

        let id: i64 = match bus.path_param("id") {
            Ok(id) => id,
            Err(error) => return Outcome::Fault(GovernanceError::Validation(error)),
        };

        let existing = match fetch_request(&resources.pool, id).await {
            Ok(Some(record)) => record,
            Ok(None) => return Outcome::Fault(GovernanceError::NotFound(id)),
            Err(error) => return Outcome::Fault(error),
        };

        if existing.status == "approved" {
            return Outcome::Fault(GovernanceError::Validation(
                "request already approved".to_string(),
            ));
        }

        let result = sqlx::query(
            r#"
            UPDATE requests
            SET status = ?, approved_by = ?
            WHERE id = ?
            "#,
        )
        .bind("approved")
        .bind(&claims.sub)
        .bind(id)
        .execute(&resources.pool)
        .await;

        if let Err(error) = result {
            return Outcome::Fault(GovernanceError::Internal(format!(
                "approve request failed: {error}"
            )));
        }

        if let Err(error) = insert_audit_event(
            &resources.pool,
            "request.approved",
            Some(id),
            &claims.sub,
            "approved by policy",
        )
        .await
        {
            return Outcome::Fault(error);
        }

        let updated = match fetch_request(&resources.pool, id).await {
            Ok(Some(record)) => record,
            Ok(None) => {
                return Outcome::Fault(GovernanceError::Internal(format!(
                    "approved request missing: {id}"
                )));
            }
            Err(error) => return Outcome::Fault(error),
        };

        Outcome::Next(
            serde_json::to_value(ApprovalResult {
                approved: true,
                request: updated,
            })
            .unwrap_or_else(|_| serde_json::json!({})),
        )
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .try_init()
        .map_err(|e| anyhow::anyhow!("tracing init failed: {e}"))?;

    let addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:3140".to_string());
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .map_err(|e| anyhow::anyhow!("sqlite connect failed: {e}"))?;

    initialize_db(&pool)
        .await
        .map_err(|e| anyhow::anyhow!("db init failed: {e}"))?;

    let state = AppState { pool };

    let login =
        Axon::<LoginRequest, LoginRequest, GovernanceError, AppState>::new("login").then(Login);
    let create_request =
        Axon::<CreateRequestInput, CreateRequestInput, GovernanceError, AppState>::new(
            "create-request",
        )
        .then(CreateRequest);
    let get_request =
        Axon::<(), (), GovernanceError, AppState>::new("get-request").then(GetRequest);
    let approve_request =
        Axon::<(), (), GovernanceError, AppState>::new("approve-request").then(ApproveRequest);

    println!("Request Governance Demo listening on http://{addr}");
    println!("  POST /login");
    println!("  POST /requests");
    println!("  GET  /requests/:id");
    println!("  POST /requests/:id/approve");
    println!("Users: admin/admin123, alice/alice123");

    Ranvier::http::<AppState>()
        .bind(&addr)
        .bus_injector(move |parts, bus| {
            let headers = parts
                .headers
                .iter()
                .filter_map(|(key, value)| {
                    value
                        .to_str()
                        .ok()
                        .map(|value| (key.as_str().to_ascii_lowercase(), value.to_string()))
                })
                .collect::<HashMap<_, _>>();
            bus.insert(RequestHeaders(headers));
            if let Some(params) = parts.extensions.get::<PathParams>() {
                bus.insert(params.clone());
            }
        })
        .guard(AccessLogGuard::<AppState>::new())
        .post_typed_json_out("/login", login)
        .post_typed_json_out("/requests", create_request)
        .get_with_error("/requests/:id", get_request, governance_error_response)
        .post_with_error(
            "/requests/:id/approve",
            approve_request,
            governance_error_response,
        )
        .run(state)
        .await
}
