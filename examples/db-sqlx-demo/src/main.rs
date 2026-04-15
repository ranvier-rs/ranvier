//! # DB SQLx Demo
//!
//! Demonstrates injecting a SQLx pool via Bus and executing queries in Transitions.
//! Replaces the removed `ranvier-db` crate for SQLx usage.
//!
//! ## Run
//! ```bash
//! # Requires SQLite (no server needed):
//! cargo run -p db-sqlx-demo
//! ```
//!
//! ## Key Concepts
//! - SQLx Pool as a Bus-injectable resource
//! - Query execution inside Transitions
//! - Optional local `safe_query_builder.rs` helper for dynamic SQL safety
//! - No public DB wrapper crate needed — `sqlx` + Bus injection is sufficient

mod safe_query_builder;

use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_core::transition::ResourceRequirement;
use ranvier_runtime::Axon;
use safe_query_builder::{QueryBuilder, SortDirection};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Row, SqlitePool};

// ============================================================================
// Resources
// ============================================================================

#[derive(Clone)]
struct DbPool(SqlitePool);

impl ResourceRequirement for DbPool {}

// ============================================================================
// Domain Types
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CreateUserReq {
    name: String,
    email: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct UserRecord {
    id: i64,
    name: String,
    email: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct UserList {
    users: Vec<UserRecord>,
}

// ============================================================================
// Transitions
// ============================================================================

#[derive(Clone)]
struct InitSchema;

#[async_trait]
impl Transition<(), ()> for InitSchema {
    type Error = String;
    type Resources = DbPool;

    async fn run(
        &self,
        _input: (),
        resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<(), Self::Error> {
        match sqlx::query(
            "CREATE TABLE IF NOT EXISTS users (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                email TEXT NOT NULL
            )",
        )
        .execute(&resources.0)
        .await
        {
            Ok(_) => Outcome::next(()),
            Err(e) => Outcome::fault(format!("Schema init failed: {}", e)),
        }
    }
}

#[derive(Clone)]
struct InsertUser;

#[async_trait]
impl Transition<CreateUserReq, UserRecord> for InsertUser {
    type Error = String;
    type Resources = DbPool;

    async fn run(
        &self,
        input: CreateUserReq,
        resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<UserRecord, Self::Error> {
        let result = match sqlx::query("INSERT INTO users (name, email) VALUES (?, ?)")
            .bind(&input.name)
            .bind(&input.email)
            .execute(&resources.0)
            .await
        {
            Ok(r) => r,
            Err(e) => return Outcome::fault(format!("Insert failed: {}", e)),
        };

        let id = result.last_insert_rowid();
        println!("  [DB] Inserted user id={} name={}", id, input.name);

        Outcome::next(UserRecord {
            id,
            name: input.name,
            email: input.email,
        })
    }
}

#[derive(Clone)]
struct ListUsers;

#[async_trait]
impl Transition<(), UserList> for ListUsers {
    type Error = String;
    type Resources = DbPool;

    async fn run(
        &self,
        _input: (),
        resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<UserList, Self::Error> {
        let query = QueryBuilder::new("SELECT id, name, email FROM users")
            .order_by("id", SortDirection::Asc)
            .build();

        let mut statement = sqlx::query(&query.sql);
        for value in query.bindings {
            statement = match value {
                safe_query_builder::SqlValue::Int(v) => statement.bind(v),
                safe_query_builder::SqlValue::Float(v) => statement.bind(v),
                safe_query_builder::SqlValue::Text(v) => statement.bind(v),
                safe_query_builder::SqlValue::Bool(v) => statement.bind(v),
                safe_query_builder::SqlValue::Null => {
                    unreachable!("QueryBuilder::filter renders NULL as IS NULL without bindings")
                }
            };
        }

        let rows = match statement.fetch_all(&resources.0).await {
            Ok(r) => r,
            Err(e) => return Outcome::fault(format!("Query failed: {}", e)),
        };

        let users: Vec<UserRecord> = rows
            .iter()
            .map(|row| UserRecord {
                id: row.get("id"),
                name: row.get("name"),
                email: row.get("email"),
            })
            .collect();

        Outcome::next(UserList { users })
    }
}

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== DB SQLx Demo ===\n");

    // In-memory SQLite — no external database needed
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect("sqlite::memory:")
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    let db = DbPool(pool);

    // ── Init schema ──────────────────────────────────────────────────────

    println!("--- Init schema ---");
    let init_axon = Axon::<(), (), String, DbPool>::new("init-schema").then(InitSchema);
    let mut bus = Bus::new();
    init_axon.execute((), &db, &mut bus).await;
    println!("  Schema created\n");

    // ── Insert users ─────────────────────────────────────────────────────

    println!("--- Insert users ---");
    let insert_axon =
        Axon::<CreateUserReq, CreateUserReq, String, DbPool>::new("insert-user").then(InsertUser);

    for (name, email) in [
        ("Alice", "alice@example.com"),
        ("Bob", "bob@example.com"),
        ("Charlie", "charlie@example.com"),
    ] {
        let mut bus = Bus::new();
        insert_axon
            .execute(
                CreateUserReq {
                    name: name.into(),
                    email: email.into(),
                },
                &db,
                &mut bus,
            )
            .await;
    }

    // ── List users ───────────────────────────────────────────────────────

    println!("\n--- List users ---");
    let list_axon = Axon::<(), (), String, DbPool>::new("list-users").then(ListUsers);
    let mut bus = Bus::new();
    let result = list_axon.execute((), &db, &mut bus).await;

    match &result {
        Outcome::Next(list) => {
            for user in &list.users {
                println!("  id={} name={} email={}", user.id, user.name, user.email);
            }
        }
        other => println!("  {:?}", other),
    }

    println!("\ndone");
    Ok(())
}
