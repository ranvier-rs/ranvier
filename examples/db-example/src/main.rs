// Database Integration Example for Ranvier
//
// This example demonstrates how to use ranvier-db to perform
// database operations within an Axon chain.

use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_db::prelude::*;
use serde::{Deserialize, Serialize};

// ============== Domain Types ==============

/// User ID wrapper
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserId(pub i32);

/// User domain model
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct User {
    pub id: i32,
    pub username: String,
    pub email: String,
    pub created_at: String,
}

/// User creation request
#[derive(Debug, Clone)]
pub struct CreateUserRequest {
    pub username: String,
    pub email: String,
}

// ============== Database Transitions ==============

/// Transition: Get user by ID
#[derive(Clone, Copy)]
pub struct GetUserById;

#[async_trait]
impl DbTransition<UserId, User> for GetUserById {
    type Error = anyhow::Error;

    async fn run(&self, input: UserId, pool: &sqlx::PgPool) -> QueryResult<User> {
        sqlx::query_as::<_, User>("SELECT id, username, email, created_at FROM users WHERE id = $1")
            .bind(input.0)
            .fetch_one(pool)
            .await
            .map_err(|e| DbError::QueryFailed(e.to_string()))
    }
}

/// Transition: Create new user
#[derive(Clone, Copy)]
pub struct CreateUser;

#[async_trait]
impl DbTransition<CreateUserRequest, User> for CreateUser {
    type Error = anyhow::Error;

    async fn run(&self, input: CreateUserRequest, pool: &sqlx::PgPool) -> QueryResult<User> {
        sqlx::query_as::<_, User>(
            "INSERT INTO users (username, email) VALUES ($1, $2) RETURNING id, username, email, created_at"
        )
        .bind(&input.username)
        .bind(&input.email)
        .fetch_one(pool)
        .await
        .map_err(|e| DbError::QueryFailed(e.to_string()))
    }
}

/// Transition: List all users
#[derive(Clone, Copy)]
pub struct ListUsers;

#[async_trait]
impl DbTransition<(), Vec<User>> for ListUsers {
    type Error = anyhow::Error;

    async fn run(&self, _input: (), pool: &sqlx::PgPool) -> QueryResult<Vec<User>> {
        sqlx::query_as::<_, User>("SELECT id, username, email, created_at FROM users ORDER BY id")
            .fetch_all(pool)
            .await
            .map_err(|e| DbError::QueryFailed(e.to_string()))
    }
}

// ============== Main Entry Point ==============

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize PostgreSQL connection pool
    // In production, use environment variables
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:password@localhost/ranvier_example".to_string());

    println!("🔌 Connecting to database...");

    let pool = PostgresPool::new(&database_url).await?;
    println!("✅ Database connected!");

    // Create a test table if it doesn't exist
    setup_schema(&pool).await?;

    // Store pool on the Bus
    let mut bus = Bus::new(http::Request::builder().uri("/").body(()).unwrap());
    bus.write(pool);

    // Example 1: Create a user
    println!("\n📝 Creating user...");
    let create_request = CreateUserRequest {
        username: "alice".to_string(),
        email: "alice@example.com".to_string(),
    };

    let result = Axon::start(create_request, "create_user")
        .then(PgNode::new(CreateUser))
        .execute(&mut bus)
        .await?;

    match result {
        Outcome::Next(user) => {
            println!("✅ User created: {} ({})", user.username, user.email);
        }
        Outcome::Fault(e) => {
            println!("❌ Failed to create user: {:?}", e);
        }
        _ => {}
    }

    // Example 2: Get user by ID
    println!("\n🔍 Looking up user by ID (1)...");
    let result = Axon::start(UserId(1), "get_user")
        .then(PgNode::new(GetUserById))
        .execute(&mut bus)
        .await?;

    match result {
        Outcome::Next(user) => {
            println!("✅ Found user: {} (ID: {})", user.username, user.id);
        }
        Outcome::Fault(e) => {
            println!("❌ Failed to get user: {:?}", e);
        }
        _ => {}
    }

    // Example 3: List all users
    println!("\n📋 Listing all users...");
    let result = Axon::start((), "list_users")
        .then(PgNode::new(ListUsers))
        .execute(&mut bus)
        .await?;

    match result {
        Outcome::Next(users) => {
            println!("✅ Found {} users:", users.len());
            for user in users {
                println!("   - {} ({}) [ID: {}]", user.username, user.email, user.id);
            }
        }
        Outcome::Fault(e) => {
            println!("❌ Failed to list users: {:?}", e);
        }
        _ => {}
    }

    println!("\n✨ Example completed!");

    Ok(())
}

/// Setup database schema (for demo purposes)
async fn setup_schema(pool: &PostgresPool) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS users (
            id SERIAL PRIMARY KEY,
            username VARCHAR(100) NOT NULL UNIQUE,
            email VARCHAR(255) NOT NULL,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .execute(pool.inner())
    .await?;

    println!("✅ Database schema ready!");
    Ok(())
}
