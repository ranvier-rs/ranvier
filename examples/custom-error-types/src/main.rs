//! Custom Error Types Example
//!
//! ## Purpose
//! Demonstrates how to use domain-specific error types with `thiserror` instead of `String`,
//! enabling type-safe error handling and structured error reporting.
//!
//! ## Run
//! ```bash
//! cargo run -p custom-error-types
//! ```
//!
//! ## Key Concepts
//! - Using `thiserror::Error` for ergonomic error enums
//! - Typed errors as `Transition::Error` associated type
//! - Matching on specific error variants
//! - `Outcome::Fault` with custom error types
//!
//! ## Prerequisites
//! - `hello-world` — basic Transition + Axon usage
//! - `testing-patterns` — unit/integration testing strategies
//!
//! ## Next Steps
//! - `retry-dlq-demo` — retry, timeout, and DLQ patterns
//! - `order-processing-demo` — production-style workflow with branching

use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_runtime::Axon;
use serde::{Deserialize, Serialize};

// ============================================================================
// Domain Error Type
// ============================================================================

#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
enum AppError {
    #[error("validation error: {0}")]
    Validation(String),
    #[error("resource not found: {0}")]
    NotFound(String),
    #[error("unauthorized access")]
    Unauthorized,
}

// ============================================================================
// Domain Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UserId(String);

#[derive(Debug, Clone, Serialize, Deserialize)]
struct User {
    id: UserId,
    name: String,
    role: String,
}

// ============================================================================
// Transitions with Typed Errors
// ============================================================================

#[derive(Clone)]
struct ValidateUserId;

#[async_trait]
impl Transition<UserId, UserId> for ValidateUserId {
    type Error = AppError;
    type Resources = ();

    async fn run(
        &self,
        id: UserId,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<UserId, Self::Error> {
        if id.0.is_empty() {
            return Outcome::Fault(AppError::Validation(
                "User ID cannot be empty".into(),
            ));
        }
        if id.0.len() < 3 {
            return Outcome::Fault(AppError::Validation(
                "User ID must be at least 3 characters".into(),
            ));
        }
        Outcome::Next(id)
    }
}

#[derive(Clone)]
struct FetchUser;

#[async_trait]
impl Transition<UserId, User> for FetchUser {
    type Error = AppError;
    type Resources = ();

    async fn run(
        &self,
        id: UserId,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<User, Self::Error> {
        match id.0.as_str() {
            "admin" => Outcome::Next(User {
                id: id.clone(),
                name: "Admin User".into(),
                role: "admin".into(),
            }),
            "user123" => Outcome::Next(User {
                id: id.clone(),
                name: "Regular User".into(),
                role: "user".into(),
            }),
            _ => Outcome::Fault(AppError::NotFound(format!("User '{}' not found", id.0))),
        }
    }
}

#[derive(Clone)]
struct CheckAuthorization;

#[async_trait]
impl Transition<User, User> for CheckAuthorization {
    type Error = AppError;
    type Resources = ();

    async fn run(
        &self,
        user: User,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<User, Self::Error> {
        if user.role != "admin" {
            return Outcome::Fault(AppError::Unauthorized);
        }
        Outcome::Next(user)
    }
}

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Custom Error Types Example ===\n");

    let mut bus = Bus::new();

    // Test 1: Successful flow
    println!("Test 1: Valid admin user");
    let axon = Axon::<UserId, UserId, AppError, ()>::new("auth.pipeline")
        .then(ValidateUserId)
        .then(FetchUser)
        .then(CheckAuthorization);

    match axon.execute(UserId("admin".into()), &(), &mut bus).await {
        Outcome::Next(user) => {
            println!("  Success: {} (role={})", user.name, user.role);
        }
        Outcome::Fault(err) => {
            println!("  Error: {}", err);
        }
        _ => {}
    }

    // Test 2: Validation error
    println!("\nTest 2: Invalid user ID (too short)");
    let axon = Axon::<UserId, UserId, AppError, ()>::new("auth.validate")
        .then(ValidateUserId)
        .then(FetchUser);

    match axon.execute(UserId("ab".into()), &(), &mut bus).await {
        Outcome::Next(user) => {
            println!("  Success: {:?}", user);
        }
        Outcome::Fault(err) => {
            println!("  Error: {}", err);
            match &err {
                AppError::Validation(msg) => println!("  -> Validation detail: {}", msg),
                _ => println!("  -> Unexpected error variant"),
            }
        }
        _ => {}
    }

    // Test 3: Not found error
    println!("\nTest 3: Non-existent user");
    let axon = Axon::<UserId, UserId, AppError, ()>::new("auth.lookup")
        .then(ValidateUserId)
        .then(FetchUser);

    match axon.execute(UserId("unknown".into()), &(), &mut bus).await {
        Outcome::Next(user) => {
            println!("  Success: {:?}", user);
        }
        Outcome::Fault(err) => {
            println!("  Error: {}", err);
            match &err {
                AppError::NotFound(resource) => println!("  -> Not found: {}", resource),
                _ => println!("  -> Unexpected error variant"),
            }
        }
        _ => {}
    }

    // Test 4: Authorization error
    println!("\nTest 4: Unauthorized user");
    let axon = Axon::<UserId, UserId, AppError, ()>::new("auth.full")
        .then(ValidateUserId)
        .then(FetchUser)
        .then(CheckAuthorization);

    match axon.execute(UserId("user123".into()), &(), &mut bus).await {
        Outcome::Next(user) => {
            println!("  Success: {:?}", user);
        }
        Outcome::Fault(err) => {
            println!("  Error: {}", err);
            match &err {
                AppError::Unauthorized => println!("  -> Access denied"),
                _ => println!("  -> Unexpected error variant"),
            }
        }
        _ => {}
    }

    Ok(())
}
