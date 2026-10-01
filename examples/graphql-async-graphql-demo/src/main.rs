//! # GraphQL async-graphql Demo
//!
//! Demonstrates using `async-graphql` directly with Ranvier Axon pipelines.
//! Replaces the removed `ranvier-graphql` wrapper crate.
//!
//! ## Run
//! ```bash
//! cargo run -p graphql-async-graphql-demo
//! ```
//!
//! ## Key Concepts
//! - Build an `async-graphql` Schema with Query and Mutation root types
//! - Execute GraphQL queries inside a Transition (no wrapper crate needed)
//! - Use Bus to inject the Schema and pass query strings

use async_graphql::{EmptySubscription, Object, Schema, SimpleObject};
use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_runtime::{Axon, ExecutionReport};
use serde::{Deserialize, Serialize};

// ============================================================================
// GraphQL Schema Definition
// ============================================================================

#[derive(SimpleObject, Clone, Debug, Serialize, Deserialize)]
struct User {
    id: u32,
    name: String,
    email: String,
}

struct QueryRoot;

#[Object]
impl QueryRoot {
    async fn user(&self, id: u32) -> User {
        User {
            id,
            name: format!("User-{}", id),
            email: format!("user{}@example.com", id),
        }
    }

    async fn users(&self) -> Vec<User> {
        vec![
            User {
                id: 1,
                name: "Alice".into(),
                email: "alice@example.com".into(),
            },
            User {
                id: 2,
                name: "Bob".into(),
                email: "bob@example.com".into(),
            },
        ]
    }
}

struct MutationRoot;

#[Object]
impl MutationRoot {
    async fn create_user(&self, name: String, email: String) -> User {
        // Synthetic demonstration only: no database write or durable identity.
        User {
            id: 42,
            name,
            email,
        }
    }
}

type AppSchema = Schema<QueryRoot, MutationRoot, EmptySubscription>;

fn demo_schema() -> AppSchema {
    Schema::build(QueryRoot, MutationRoot, EmptySubscription)
        .limit_depth(8)
        .limit_complexity(100)
        .finish()
}

// ============================================================================
// GraphQL Executor Transition
// ============================================================================

/// Bus-injectable GraphQL schema handle.
#[derive(Clone)]
struct GraphQlSchema(AppSchema);

/// Transition that executes a GraphQL query string using the schema from Bus.
#[derive(Clone)]
struct ExecuteGraphQl;

#[async_trait]
impl Transition<String, serde_json::Value> for ExecuteGraphQl {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        query: String,
        _resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<serde_json::Value, Self::Error> {
        if query.len() > 16_384 {
            return Outcome::fault("GraphQL request exceeds local size limit".into());
        }
        let Some(schema) = bus.read::<GraphQlSchema>() else {
            return Outcome::fault("GraphQL schema unavailable".into());
        };

        let request = async_graphql::Request::new(&query);
        let response = schema.0.execute(request).await;
        let data = match serde_json::to_value(&response.data) {
            Ok(v) => v,
            Err(_) => return Outcome::fault("GraphQL response encoding failed".into()),
        };
        let mut envelope = serde_json::json!({ "data": data });
        if !response.errors.is_empty() {
            // Do not expose resolver errors, request text or extensions.
            envelope["errors"] = serde_json::json!([
                { "message": "GraphQL request failed" }
            ]);
        }
        Outcome::next(envelope)
    }
}

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== GraphQL async-graphql Demo ===\n");

    let schema = demo_schema();

    let axon = Axon::<String, String, String>::new("GraphQL Executor").then(ExecuteGraphQl);

    // ── Query: list users ────────────────────────────────────────────────

    println!("--- Query: {{ users {{ id name email }} }} ---");
    {
        let mut bus = Bus::new();
        bus.insert(GraphQlSchema(schema.clone()));
        let result = axon
            .execute("{ users { id name email } }".into(), &(), &mut bus)
            .await;
        println!(
            "  Response: {}\n",
            serde_json::to_string_pretty(&local_demo_response(result)?)?
        );
    }

    // ── Query: single user ───────────────────────────────────────────────

    println!("--- Query: {{ user(id: 1) {{ name }} }} ---");
    {
        let mut bus = Bus::new();
        bus.insert(GraphQlSchema(schema.clone()));
        let result = axon
            .execute("{ user(id: 1) { name } }".into(), &(), &mut bus)
            .await;
        println!(
            "  Response: {}\n",
            serde_json::to_string_pretty(&local_demo_response(result)?)?
        );
    }

    // ── Mutation: create user ────────────────────────────────────────────

    println!("--- Mutation: createUser ---");
    {
        let mut bus = Bus::new();
        bus.insert(GraphQlSchema(schema.clone()));
        let result = axon
            .execute(
                r#"mutation { createUser(name: "Charlie", email: "charlie@example.com") { id name email } }"#.into(),
                &(),
                &mut bus,
            )
            .await;
        println!(
            "  Response: {}\n",
            serde_json::to_string_pretty(&local_demo_response(result)?)?
        );
    }

    println!("done");
    Ok(())
}

fn local_demo_response(
    report: ExecutionReport<Outcome<serde_json::Value, String>>,
) -> anyhow::Result<serde_json::Value> {
    if report
        .cleanup()
        .is_some_and(|cleanup| !cleanup.is_complete())
    {
        anyhow::bail!("Execution requires reconciliation before retry");
    }
    match report {
        ExecutionReport::Unpersisted {
            result: Outcome::Next(response),
            ..
        } => Ok(response),
        _ => anyhow::bail!("Execution requires reconciliation before retry"),
    }
}
