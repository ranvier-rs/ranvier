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
use ranvier_runtime::Axon;
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
        User {
            id: 42,
            name,
            email,
        }
    }
}

type AppSchema = Schema<QueryRoot, MutationRoot, EmptySubscription>;

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
        let schema = bus
            .read::<GraphQlSchema>()
            .expect("GraphQlSchema must be in Bus");

        let request = async_graphql::Request::new(&query);
        let response = schema.0.execute(request).await;
        let json = match serde_json::to_value(&response.data) {
            Ok(v) => v,
            Err(e) => return Outcome::fault(format!("serialize error: {}", e)),
        };

        Outcome::next(json)
    }
}

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== GraphQL async-graphql Demo ===\n");

    let schema = Schema::build(QueryRoot, MutationRoot, EmptySubscription).finish();

    let axon = Axon::<String, String, String>::new("GraphQL Executor").then(ExecuteGraphQl);

    // ── Query: list users ────────────────────────────────────────────────

    println!("--- Query: {{ users {{ id name email }} }} ---");
    {
        let mut bus = Bus::new();
        bus.insert(GraphQlSchema(schema.clone()));
        let result = axon
            .execute("{ users { id name email } }".into(), &(), &mut bus)
            .await;
        match &result {
            Outcome::Next(data) => println!("  Result: {}\n", serde_json::to_string_pretty(data)?),
            other => println!("  {:?}\n", other),
        }
    }

    // ── Query: single user ───────────────────────────────────────────────

    println!("--- Query: {{ user(id: 1) {{ name }} }} ---");
    {
        let mut bus = Bus::new();
        bus.insert(GraphQlSchema(schema.clone()));
        let result = axon
            .execute("{ user(id: 1) { name } }".into(), &(), &mut bus)
            .await;
        match &result {
            Outcome::Next(data) => println!("  Result: {}\n", serde_json::to_string_pretty(data)?),
            other => println!("  {:?}\n", other),
        }
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
        match &result {
            Outcome::Next(data) => println!("  Result: {}\n", serde_json::to_string_pretty(data)?),
            other => println!("  {:?}\n", other),
        }
    }

    println!("done");
    Ok(())
}
