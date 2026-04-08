//! Reference Fullstack Admin
//!
//! ## Purpose
//! Public-only fullstack reference app with a Ranvier backend and a SvelteKit
//! frontend for a small admin surface.
//!
//! ## Run
//! ```bash
//! cargo run -p reference-fullstack-admin
//! ```
//!
//! ## Key Concepts
//! - Fullstack-oriented backend surface with JWT login and CORS
//! - SQLite-backed admin data with explicit Axon transitions
//! - OpenAPI + Swagger UI for the backend contract
//! - Public-only reference app boundary independent of `playground/`
//!
//! ## Prerequisites
//! - `admin-crud-demo` — bridge backend example
//! - `openapi-demo` — OpenAPI generation baseline
//!
//! ## Next Steps
//! - `request-governance-demo` — auth/guard/audit/error/governance wiring

use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use http::Method;
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use ranvier_core::prelude::*;
use ranvier_core::transition::ResourceRequirement;
use ranvier_guard::prelude::*;
use ranvier_http::prelude::*;
use ranvier_openapi::prelude::*;
use ranvier_runtime::Axon;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::{sqlite::SqlitePoolOptions, Row, SqlitePool};

static JWT_SECRET: LazyLock<String> =
    LazyLock::new(|| std::env::var("JWT_SECRET").unwrap_or_else(|_| "reference-fullstack-admin-secret".to_string()));
static ADMIN_PASSWORD: LazyLock<String> =
    LazyLock::new(|| std::env::var("ADMIN_PASSWORD").unwrap_or_else(|_| "admin123".to_string()));

#[derive(Clone)]
struct RequestHeaders(HashMap<String, String>);

#[derive(Clone)]
struct AppState {
    pool: SqlitePool,
    openapi_json: serde_json::Value,
    swagger_html: String,
}

impl ResourceRequirement for AppState {}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct LoginRequest {
    username: String,
    password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct LoginResponse {
    token: String,
    username: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct DashboardSummary {
    active_users: i64,
    departments: i64,
    total_users: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct Department {
    id: i64,
    name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct UserRecord {
    id: i64,
    username: String,
    full_name: String,
    email: String,
    department_id: i64,
    department_name: String,
    active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct UserPage {
    items: Vec<UserRecord>,
    page: i64,
    per_page: i64,
    total: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct CreateUserInput {
    username: String,
    full_name: String,
    email: String,
    department_id: i64,
    active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct UpdateUserInput {
    full_name: Option<String>,
    email: Option<String>,
    department_id: Option<i64>,
    active: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct DeleteUserResult {
    deleted: bool,
    id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct DocsPage {
    html: String,
}

impl IntoResponse for DocsPage {
    fn into_response(self) -> HttpResponse {
        Html(self.html).into_response()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Claims {
    sub: String,
    exp: usize,
}

fn issue_token(username: &str) -> Result<String, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("clock error: {e}"))?
        .as_secs() as usize;

    let claims = Claims {
        sub: username.to_string(),
        exp: now + 24 * 60 * 60,
    };

    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(JWT_SECRET.as_bytes()),
    )
    .map_err(|e| format!("token encode failed: {e}"))
}

fn verify_token(token: &str) -> Result<Claims, String> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_exp = true;

    decode::<Claims>(
        token,
        &DecodingKey::from_secret(JWT_SECRET.as_bytes()),
        &validation,
    )
    .map(|data| data.claims)
    .map_err(|e| format!("token verify failed: {e}"))
}

fn bearer_from_headers(headers: &RequestHeaders) -> Option<&str> {
    headers
        .0
        .get("authorization")
        .and_then(|value| value.strip_prefix("Bearer "))
}

fn require_admin(bus: &mut Bus) -> Result<Claims, String> {
    let headers = bus
        .get_cloned::<RequestHeaders>()
        .map_err(|_| "request headers missing".to_string())?;
    let token = bearer_from_headers(&headers).ok_or_else(|| "missing Bearer token".to_string())?;
    let claims = verify_token(token)?;
    if claims.sub != "admin" {
        return Err("admin access required".to_string());
    }
    Ok(claims)
}

fn normalize_page(page: i64) -> i64 {
    if page < 1 { 1 } else { page }
}

fn normalize_per_page(per_page: i64) -> i64 {
    per_page.clamp(1, 100)
}

fn row_to_department(row: &sqlx::sqlite::SqliteRow) -> Result<Department, String> {
    Ok(Department {
        id: row.try_get("id").map_err(|e| format!("department.id decode failed: {e}"))?,
        name: row.try_get("name").map_err(|e| format!("department.name decode failed: {e}"))?,
    })
}

fn row_to_user(row: &sqlx::sqlite::SqliteRow) -> Result<UserRecord, String> {
    let active_int: i64 = row.try_get("active").map_err(|e| format!("user.active decode failed: {e}"))?;
    Ok(UserRecord {
        id: row.try_get("id").map_err(|e| format!("user.id decode failed: {e}"))?,
        username: row.try_get("username").map_err(|e| format!("user.username decode failed: {e}"))?,
        full_name: row.try_get("full_name").map_err(|e| format!("user.full_name decode failed: {e}"))?,
        email: row.try_get("email").map_err(|e| format!("user.email decode failed: {e}"))?,
        department_id: row.try_get("department_id").map_err(|e| format!("user.department_id decode failed: {e}"))?,
        department_name: row.try_get("department_name").map_err(|e| format!("user.department_name decode failed: {e}"))?,
        active: active_int != 0,
    })
}

async fn fetch_user_by_id(pool: &SqlitePool, id: i64) -> Result<Option<UserRecord>, String> {
    let row = sqlx::query(
        r#"
        SELECT
            users.id,
            users.username,
            users.full_name,
            users.email,
            users.department_id,
            departments.name AS department_name,
            users.active
        FROM users
        JOIN departments ON departments.id = users.department_id
        WHERE users.id = ?
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("fetch user failed: {e}"))?;

    match row {
        Some(row) => row_to_user(&row).map(Some),
        None => Ok(None),
    }
}

async fn initialize_db(pool: &SqlitePool) -> Result<(), String> {
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(pool)
        .await
        .map_err(|e| format!("pragma failed: {e}"))?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS departments (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL
        )
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| format!("create departments failed: {e}"))?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS users (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            username TEXT NOT NULL UNIQUE,
            full_name TEXT NOT NULL,
            email TEXT NOT NULL,
            department_id INTEGER NOT NULL,
            active INTEGER NOT NULL DEFAULT 1,
            FOREIGN KEY (department_id) REFERENCES departments(id)
        )
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| format!("create users failed: {e}"))?;

    let dept_count: i64 = sqlx::query("SELECT COUNT(*) AS count FROM departments")
        .fetch_one(pool)
        .await
        .map_err(|e| format!("count departments failed: {e}"))?
        .try_get("count")
        .map_err(|e| format!("department count decode failed: {e}"))?;

    if dept_count == 0 {
        for name in ["Platform", "Operations", "Security"] {
            sqlx::query("INSERT INTO departments (name) VALUES (?)")
                .bind(name)
                .execute(pool)
                .await
                .map_err(|e| format!("seed department failed: {e}"))?;
        }
    }

    let user_count: i64 = sqlx::query("SELECT COUNT(*) AS count FROM users")
        .fetch_one(pool)
        .await
        .map_err(|e| format!("count users failed: {e}"))?
        .try_get("count")
        .map_err(|e| format!("user count decode failed: {e}"))?;

    if user_count == 0 {
        for (username, full_name, email, department_id) in [
            ("alice", "Alice Admin", "alice@example.com", 1_i64),
            ("bob", "Bob Operator", "bob@example.com", 2_i64),
        ] {
            sqlx::query(
                r#"
                INSERT INTO users (username, full_name, email, department_id, active)
                VALUES (?, ?, ?, ?, ?)
                "#,
            )
            .bind(username)
            .bind(full_name)
            .bind(email)
            .bind(department_id)
            .bind(1_i64)
            .execute(pool)
            .await
            .map_err(|e| format!("seed user failed: {e}"))?;
        }
    }

    Ok(())
}

#[derive(Clone, Copy)]
struct Login;

#[async_trait]
impl Transition<LoginRequest, LoginResponse> for Login {
    type Error = String;
    type Resources = AppState;

    async fn run(&self, input: LoginRequest, _resources: &Self::Resources, _bus: &mut Bus) -> Outcome<LoginResponse, Self::Error> {
        if input.username != "admin" || input.password != *ADMIN_PASSWORD {
            return Outcome::Fault("invalid credentials".to_string());
        }

        match issue_token(&input.username) {
            Ok(token) => Outcome::Next(LoginResponse { token, username: input.username }),
            Err(error) => Outcome::Fault(error),
        }
    }
}

#[derive(Clone, Copy)]
struct Dashboard;

#[async_trait]
impl Transition<(), DashboardSummary> for Dashboard {
    type Error = String;
    type Resources = AppState;

    async fn run(&self, _input: (), resources: &Self::Resources, bus: &mut Bus) -> Outcome<DashboardSummary, Self::Error> {
        if let Err(error) = require_admin(bus) {
            return Outcome::Fault(error);
        }

        let total_users: i64 = match sqlx::query("SELECT COUNT(*) AS count FROM users")
            .fetch_one(&resources.pool)
            .await
        {
            Ok(row) => match row.try_get("count") {
                Ok(value) => value,
                Err(error) => return Outcome::Fault(format!("dashboard total decode failed: {error}")),
            },
            Err(error) => return Outcome::Fault(format!("dashboard total failed: {error}")),
        };

        let active_users: i64 = match sqlx::query("SELECT COUNT(*) AS count FROM users WHERE active = 1")
            .fetch_one(&resources.pool)
            .await
        {
            Ok(row) => match row.try_get("count") {
                Ok(value) => value,
                Err(error) => return Outcome::Fault(format!("dashboard active decode failed: {error}")),
            },
            Err(error) => return Outcome::Fault(format!("dashboard active failed: {error}")),
        };

        let departments: i64 = match sqlx::query("SELECT COUNT(*) AS count FROM departments")
            .fetch_one(&resources.pool)
            .await
        {
            Ok(row) => match row.try_get("count") {
                Ok(value) => value,
                Err(error) => return Outcome::Fault(format!("dashboard departments decode failed: {error}")),
            },
            Err(error) => return Outcome::Fault(format!("dashboard departments failed: {error}")),
        };

        Outcome::Next(DashboardSummary {
            active_users,
            departments,
            total_users,
        })
    }
}

#[derive(Clone, Copy)]
struct ListDepartments;

#[async_trait]
impl Transition<(), Vec<Department>> for ListDepartments {
    type Error = String;
    type Resources = AppState;

    async fn run(&self, _input: (), resources: &Self::Resources, bus: &mut Bus) -> Outcome<Vec<Department>, Self::Error> {
        if let Err(error) = require_admin(bus) {
            return Outcome::Fault(error);
        }

        let rows = match sqlx::query("SELECT id, name FROM departments ORDER BY name ASC")
            .fetch_all(&resources.pool)
            .await
        {
            Ok(rows) => rows,
            Err(error) => return Outcome::Fault(format!("list departments failed: {error}")),
        };

        let mut departments = Vec::with_capacity(rows.len());
        for row in rows {
            match row_to_department(&row) {
                Ok(department) => departments.push(department),
                Err(error) => return Outcome::Fault(error),
            }
        }

        Outcome::Next(departments)
    }
}

#[derive(Clone, Copy)]
struct ListUsers;

#[async_trait]
impl Transition<(), UserPage> for ListUsers {
    type Error = String;
    type Resources = AppState;

    async fn run(&self, _input: (), resources: &Self::Resources, bus: &mut Bus) -> Outcome<UserPage, Self::Error> {
        if let Err(error) = require_admin(bus) {
            return Outcome::Fault(error);
        }

        let page = normalize_page(bus.query_param_or("page", 1_i64));
        let per_page = normalize_per_page(bus.query_param_or("per_page", 10_i64));
        let q: Option<String> = bus.query_param("q");
        let offset = (page - 1) * per_page;
        let q_trimmed = q.as_ref().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let q_like = q_trimmed.as_ref().map(|v| format!("%{v}%"));

        let total = match q_like.as_ref() {
            Some(q_value) => match sqlx::query(
                "SELECT COUNT(*) AS count FROM users WHERE username LIKE ? OR full_name LIKE ? OR email LIKE ?",
            )
            .bind(q_value)
            .bind(q_value)
            .bind(q_value)
            .fetch_one(&resources.pool)
            .await
            {
                Ok(row) => match row.try_get("count") {
                    Ok(value) => value,
                    Err(error) => return Outcome::Fault(format!("count decode failed: {error}")),
                },
                Err(error) => return Outcome::Fault(format!("count users failed: {error}")),
            },
            None => match sqlx::query("SELECT COUNT(*) AS count FROM users")
                .fetch_one(&resources.pool)
                .await
            {
                Ok(row) => match row.try_get("count") {
                    Ok(value) => value,
                    Err(error) => return Outcome::Fault(format!("count decode failed: {error}")),
                },
                Err(error) => return Outcome::Fault(format!("count users failed: {error}")),
            },
        };

        let rows = match q_like.as_ref() {
            Some(q_value) => {
                sqlx::query(
                    r#"
                    SELECT
                        users.id,
                        users.username,
                        users.full_name,
                        users.email,
                        users.department_id,
                        departments.name AS department_name,
                        users.active
                    FROM users
                    JOIN departments ON departments.id = users.department_id
                    WHERE users.username LIKE ? OR users.full_name LIKE ? OR users.email LIKE ?
                    ORDER BY users.id ASC
                    LIMIT ? OFFSET ?
                    "#,
                )
                .bind(q_value)
                .bind(q_value)
                .bind(q_value)
                .bind(per_page)
                .bind(offset)
                .fetch_all(&resources.pool)
                .await
            }
            None => {
                sqlx::query(
                    r#"
                    SELECT
                        users.id,
                        users.username,
                        users.full_name,
                        users.email,
                        users.department_id,
                        departments.name AS department_name,
                        users.active
                    FROM users
                    JOIN departments ON departments.id = users.department_id
                    ORDER BY users.id ASC
                    LIMIT ? OFFSET ?
                    "#,
                )
                .bind(per_page)
                .bind(offset)
                .fetch_all(&resources.pool)
                .await
            }
        };

        let rows = match rows {
            Ok(rows) => rows,
            Err(error) => return Outcome::Fault(format!("list users failed: {error}")),
        };

        let mut items = Vec::with_capacity(rows.len());
        for row in rows {
            match row_to_user(&row) {
                Ok(user) => items.push(user),
                Err(error) => return Outcome::Fault(error),
            }
        }

        Outcome::Next(UserPage {
            items,
            page,
            per_page,
            total,
        })
    }
}

#[derive(Clone, Copy)]
struct CreateUser;

#[async_trait]
impl Transition<CreateUserInput, UserRecord> for CreateUser {
    type Error = String;
    type Resources = AppState;

    async fn run(&self, input: CreateUserInput, resources: &Self::Resources, bus: &mut Bus) -> Outcome<UserRecord, Self::Error> {
        if let Err(error) = require_admin(bus) {
            return Outcome::Fault(error);
        }

        let result = sqlx::query(
            r#"
            INSERT INTO users (username, full_name, email, department_id, active)
            VALUES (?, ?, ?, ?, ?)
            "#,
        )
        .bind(input.username)
        .bind(input.full_name)
        .bind(input.email)
        .bind(input.department_id)
        .bind(if input.active { 1_i64 } else { 0_i64 })
        .execute(&resources.pool)
        .await;

        let result = match result {
            Ok(result) => result,
            Err(error) => return Outcome::Fault(format!("create user failed: {error}")),
        };

        let id = result.last_insert_rowid();
        match fetch_user_by_id(&resources.pool, id).await {
            Ok(Some(user)) => Outcome::Next(user),
            Ok(None) => Outcome::Fault(format!("created user missing: {id}")),
            Err(error) => Outcome::Fault(error),
        }
    }
}

#[derive(Clone, Copy)]
struct UpdateUser;

#[async_trait]
impl Transition<UpdateUserInput, UserRecord> for UpdateUser {
    type Error = String;
    type Resources = AppState;

    async fn run(&self, input: UpdateUserInput, resources: &Self::Resources, bus: &mut Bus) -> Outcome<UserRecord, Self::Error> {
        if let Err(error) = require_admin(bus) {
            return Outcome::Fault(error);
        }

        let id: i64 = match bus.path_param("id") {
            Ok(id) => id,
            Err(error) => return Outcome::Fault(error),
        };

        let existing = match fetch_user_by_id(&resources.pool, id).await {
            Ok(Some(user)) => user,
            Ok(None) => return Outcome::Fault(format!("user not found: {id}")),
            Err(error) => return Outcome::Fault(error),
        };

        let full_name = input.full_name.unwrap_or(existing.full_name);
        let email = input.email.unwrap_or(existing.email);
        let department_id = input.department_id.unwrap_or(existing.department_id);
        let active = input.active.unwrap_or(existing.active);

        let result = sqlx::query(
            r#"
            UPDATE users
            SET full_name = ?, email = ?, department_id = ?, active = ?
            WHERE id = ?
            "#,
        )
        .bind(full_name)
        .bind(email)
        .bind(department_id)
        .bind(if active { 1_i64 } else { 0_i64 })
        .bind(id)
        .execute(&resources.pool)
        .await;

        if let Err(error) = result {
            return Outcome::Fault(format!("update user failed: {error}"));
        }

        match fetch_user_by_id(&resources.pool, id).await {
            Ok(Some(user)) => Outcome::Next(user),
            Ok(None) => Outcome::Fault(format!("updated user missing: {id}")),
            Err(error) => Outcome::Fault(error),
        }
    }
}

#[derive(Clone, Copy)]
struct DeleteUser;

#[async_trait]
impl Transition<(), DeleteUserResult> for DeleteUser {
    type Error = String;
    type Resources = AppState;

    async fn run(&self, _input: (), resources: &Self::Resources, bus: &mut Bus) -> Outcome<DeleteUserResult, Self::Error> {
        if let Err(error) = require_admin(bus) {
            return Outcome::Fault(error);
        }

        let id: i64 = match bus.path_param("id") {
            Ok(id) => id,
            Err(error) => return Outcome::Fault(error),
        };

        let result = sqlx::query("DELETE FROM users WHERE id = ?")
            .bind(id)
            .execute(&resources.pool)
            .await;

        match result {
            Ok(result) => Outcome::Next(DeleteUserResult {
                deleted: result.rows_affected() > 0,
                id,
            }),
            Err(error) => Outcome::Fault(format!("delete user failed: {error}")),
        }
    }
}

#[derive(Clone, Copy)]
struct ServeOpenApi;

#[async_trait]
impl Transition<(), serde_json::Value> for ServeOpenApi {
    type Error = String;
    type Resources = AppState;

    async fn run(&self, _input: (), resources: &Self::Resources, _bus: &mut Bus) -> Outcome<serde_json::Value, Self::Error> {
        Outcome::Next(resources.openapi_json.clone())
    }
}

#[derive(Clone, Copy)]
struct ServeDocs;

#[async_trait]
impl Transition<(), DocsPage> for ServeDocs {
    type Error = String;
    type Resources = AppState;

    async fn run(&self, _input: (), resources: &Self::Resources, _bus: &mut Bus) -> Outcome<DocsPage, Self::Error> {
        Outcome::Next(DocsPage {
            html: resources.swagger_html.clone(),
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .try_init()
        .map_err(|e| anyhow::anyhow!("tracing init failed: {e}"))?;

    let addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:3130".to_string());
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .map_err(|e| anyhow::anyhow!("sqlite connect failed: {e}"))?;

    initialize_db(&pool)
        .await
        .map_err(|e| anyhow::anyhow!("db init failed: {e}"))?;

    let login = Axon::<LoginRequest, LoginRequest, String, AppState>::new("login").then(Login);
    let dashboard = Axon::<(), (), String, AppState>::new("dashboard").then(Dashboard);
    let list_departments = Axon::<(), (), String, AppState>::new("list-departments").then(ListDepartments);
    let list_users = Axon::<(), (), String, AppState>::new("list-users").then(ListUsers);
    let create_user = Axon::<CreateUserInput, CreateUserInput, String, AppState>::new("create-user").then(CreateUser);
    let update_user = Axon::<UpdateUserInput, UpdateUserInput, String, AppState>::new("update-user").then(UpdateUser);
    let delete_user = Axon::<(), (), String, AppState>::new("delete-user").then(DeleteUser);
    let openapi_route = Axon::<(), (), String, AppState>::new("serve-openapi").then(ServeOpenApi);
    let docs_route = Axon::<(), (), String, AppState>::new("serve-docs").then(ServeDocs);

    let ingress = Ranvier::http::<AppState>()
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
        .guard(CorsGuard::<AppState>::permissive())
        .post_typed_json_out("/login", login)
        .get_json_out("/dashboard", dashboard)
        .get_json_out("/departments", list_departments)
        .get_json_out("/users", list_users)
        .post_typed_json_out("/users", create_user)
        .put_typed_json_out("/users/:id", update_user)
        .delete_json_out("/users/:id", delete_user)
        .get("/openapi.json", openapi_route)
        .get("/docs", docs_route);

    let openapi_json = OpenApiGenerator::from_ingress(&ingress)
        .title("Ranvier Reference Fullstack Admin")
        .version("0.46.0-dev")
        .description("Public-only fullstack reference app backend with Ranvier + SvelteKit.")
        .with_bearer_auth()
        .summary(Method::POST, "/login", "Issue JWT for the admin user")
        .summary(Method::GET, "/dashboard", "Return dashboard summary")
        .summary(Method::GET, "/departments", "List departments")
        .summary(Method::GET, "/users", "List users with pagination and search")
        .summary(Method::POST, "/users", "Create user")
        .summary(Method::PUT, "/users/:id", "Update user")
        .summary(Method::DELETE, "/users/:id", "Delete user")
        .json_response_schema::<LoginResponse>(Method::POST, "/login")
        .json_response_schema::<DashboardSummary>(Method::GET, "/dashboard")
        .json_response_schema::<Vec<Department>>(Method::GET, "/departments")
        .json_response_schema::<UserPage>(Method::GET, "/users")
        .json_response_schema::<UserRecord>(Method::POST, "/users")
        .json_response_schema::<UserRecord>(Method::PUT, "/users/:id")
        .json_response_schema::<DeleteUserResult>(Method::DELETE, "/users/:id")
        .build_json();

    let state = AppState {
        pool,
        openapi_json,
        swagger_html: swagger_ui_html("/openapi.json", "Ranvier Reference Fullstack Admin"),
    };

    println!("Reference Fullstack Admin backend listening on http://{addr}");
    println!("Frontend dev server expected on http://127.0.0.1:5176");
    println!("Default login: admin / admin123");

    ingress.run(state).await
}
