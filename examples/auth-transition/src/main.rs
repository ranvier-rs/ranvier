//! # Auth Transition Example (Ranvier Way)
//!
//! Demonstrates **Transition-based authentication** — the recommended Ranvier approach.
//!
//! ## Why This Approach
//!
//! **Bus-based context propagation**: `AuthContext` is automatically stored in the Bus
//! after successful authentication, making it available to all downstream transitions.
//!
//! **Schematic visualization**: The entire auth flow (`authenticate` → `authorize` → `handler`)
//! is represented in `schematic.json` and visible in VSCode Circuit view.
//!
//! **Testability**: Each transition can be unit-tested independently by injecting a mock Bus.
//!
//! **Composability**: Easy to insert additional steps (e.g., `check_subscription` between
//! `authorize` and `handler`) by modifying the pipeline.
//!
//! ## Run
//!
//! ```bash
//! cargo run -p auth-transition
//! ```
//!
//! ## Compare with
//!
//! - `examples/auth-tower-integration/` — Tower Service layer integration (ecosystem way)
//! - `docs/guides/auth-comparison.md` — Detailed comparison of both approaches

mod auth;

use auth::{validate_jwt, AuthContext, AuthError};
use ranvier_core::{Bus, Outcome};
use ranvier_macros::transition;
use ranvier_runtime::Axon;
use serde::{Deserialize, Serialize};
use std::env;

// ============================================================================
// Domain Types
// ============================================================================

/// HTTP response.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Response {
    status: u16,
    body: String,
}

/// Application-level errors (unions all possible errors).
#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
enum AppError {
    #[error("Auth error: {0}")]
    Auth(String),  // Can't use #[from] with serde

    #[error("Internal server error: {0}")]
    Internal(String),
}

// ============================================================================
// Transitions
// ============================================================================

/// Transition 1: Extract and validate JWT from Bus.
///
/// Expects `IamToken` in Bus (similar to auth-jwt-role-demo pattern).
/// Returns `AuthContext` which is stored in Bus.
#[transition]
async fn authenticate(_input: (), _res: &(), bus: &mut Bus) -> Outcome<AuthContext, AppError> {
    // In a real HTTP scenario, token would be extracted from request and put in Bus.
    // For this demo, we'll expect it to already be in Bus as a String.
    let token = match bus.get_cloned::<String>() {
        Ok(t) => t,
        Err(_) => return Outcome::Fault(AppError::Auth(AuthError::MissingHeader.to_string())),
    };

    // Validate JWT
    let jwt_secret = env::var("JWT_SECRET").unwrap_or_else(|_| "default-secret-key".to_string());
    let auth_ctx = match validate_jwt(&token, &jwt_secret) {
        Ok(ctx) => ctx,
        Err(e) => return Outcome::Fault(AppError::Auth(e.to_string())),
    };

    tracing::info!(
        user_id = %auth_ctx.user_id,
        roles = ?auth_ctx.roles,
        "User authenticated successfully"
    );

    // Store AuthContext in Bus for downstream transitions
    bus.insert(auth_ctx.clone());

    Outcome::Next(auth_ctx)
}

/// Transition 2: Check role-based authorization.
///
/// Reads `AuthContext` from Bus (stored by `authenticate`).
#[transition]
async fn authorize(_input: AuthContext, _res: &(), bus: &mut Bus) -> Outcome<(), AppError> {
    let auth = bus
        .get_cloned::<AuthContext>()
        .expect("AuthContext should be in Bus after authenticate");

    let required_role = "admin";

    if !auth.roles.contains(&required_role.to_string()) {
        tracing::warn!(
            user_id = %auth.user_id,
            required_role = %required_role,
            actual_roles = ?auth.roles,
            "Authorization failed: missing required role"
        );
        return Outcome::Fault(AppError::Auth(AuthError::Unauthorized(required_role.into()).to_string()));
    }

    tracing::info!(
        user_id = %auth.user_id,
        role = %required_role,
        "Authorization successful"
    );

    Outcome::Next(())
}

/// Transition 3: Protected business logic handler.
///
/// Reads `AuthContext` from Bus (stored by `authenticate`).
#[transition]
async fn protected_handler(_input: (), _res: &(), bus: &mut Bus) -> Outcome<Response, AppError> {
    let auth = bus
        .get_cloned::<AuthContext>()
        .expect("AuthContext should be in Bus");

    tracing::info!(
        user_id = %auth.user_id,
        "Protected handler executed for authenticated user"
    );

    let body = serde_json::json!({
        "message": format!("Hello, {}!", auth.user_id),
        "user_id": auth.user_id,
        "roles": auth.roles,
        "timestamp": chrono::Utc::now().to_rfc3339(),
    });

    Outcome::Next(Response {
        status: 200,
        body: body.to_string(),
    })
}

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "auth_transition=info".into()),
        )
        .init();

    tracing::info!("Starting auth-transition example (Ranvier Way)");

    // Build the authentication pipeline
    // authenticate → authorize → protected_handler
    //
    // Key insight: AuthContext flows through Bus.
    // - `authenticate` validates JWT, stores `AuthContext` in Bus
    // - `authorize` reads `AuthContext` from Bus, checks role
    // - `protected_handler` reads `AuthContext` from Bus, generates response
    let auth_pipeline = Axon::simple::<AppError>("auth-pipeline")
        .then(authenticate)
        .then(authorize)
        .then(protected_handler);

    if auth_pipeline.maybe_export_and_exit()? {
        return Ok(());
    }

    tracing::info!("Server would listen on :3000");
    tracing::info!("Example usage:");
    tracing::info!("  Valid:   Authorization: Bearer <valid_jwt>");
    tracing::info!("  Invalid: Authorization: Bearer invalid");
    tracing::info!("  Missing: (no Authorization header)");

    // For demonstration, execute with various scenarios
    demo_execution(auth_pipeline).await?;

    Ok(())
}

/// Demonstration of pipeline execution with various scenarios.
async fn demo_execution(pipeline: Axon<(), Response, AppError, ()>) -> anyhow::Result<()> {
    tracing::info!("\n=== Demo Execution ===\n");

    // Scenario 1: Valid admin token
    {
        tracing::info!("Scenario 1: Valid admin token");
        let jwt_secret = env::var("JWT_SECRET").unwrap_or_else(|_| "default-secret-key".to_string());

        // Create test token (in real app, this comes from login endpoint)
        let token = create_test_token("alice", vec!["admin".into(), "user".into()], &jwt_secret);

        let mut bus = Bus::new();
        bus.insert(token);  // Put token in Bus (simulating HTTP middleware)

        match pipeline.execute((), &(), &mut bus).await {
            Outcome::Next(response) => {
                tracing::info!("✅ Success: {}", response.body);
            }
            other => {
                tracing::error!("❌ Error: {:?}", other);
            }
        }
    }

    // Scenario 2: Valid token but missing "admin" role
    {
        tracing::info!("\nScenario 2: Valid token, no admin role");
        let jwt_secret = env::var("JWT_SECRET").unwrap_or_else(|_| "default-secret-key".to_string());

        let token = create_test_token("bob", vec!["user".into()], &jwt_secret);

        let mut bus = Bus::new();
        bus.insert(token);

        match pipeline.execute((), &(), &mut bus).await {
            Outcome::Next(_) => {
                tracing::error!("❌ Should have failed");
            }
            other => {
                tracing::info!("✅ Expected error: {:?}", other);
            }
        }
    }

    // Scenario 3: Missing token
    {
        tracing::info!("\nScenario 3: Missing token");
        let mut bus = Bus::new();  // Empty bus

        match pipeline.execute((), &(), &mut bus).await {
            Outcome::Next(_) => {
                tracing::error!("❌ Should have failed");
            }
            other => {
                tracing::info!("✅ Expected error: {:?}", other);
            }
        }
    }

    // Scenario 4: Invalid token
    {
        tracing::info!("\nScenario 4: Invalid token");
        let mut bus = Bus::new();
        bus.insert("invalid-token".to_string());

        match pipeline.execute((), &(), &mut bus).await {
            Outcome::Next(_) => {
                tracing::error!("❌ Should have failed");
            }
            other => {
                tracing::info!("✅ Expected error: {:?}", other);
            }
        }
    }

    Ok(())
}

/// Helper to create test JWT tokens (for demonstration).
fn create_test_token(user_id: &str, roles: Vec<String>, secret: &str) -> String {
    use jsonwebtoken::{encode, EncodingKey, Header};
    use serde::Serialize;

    #[derive(Serialize)]
    struct Claims {
        sub: String,
        roles: Vec<String>,
        exp: usize,
    }

    let claims = Claims {
        sub: user_id.to_string(),
        roles,
        exp: (chrono::Utc::now() + chrono::Duration::hours(24)).timestamp() as usize,
    };

    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .expect("Failed to create test token")
}
