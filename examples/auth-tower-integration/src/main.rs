//! # Auth Tower Integration Example (Ecosystem Way)
//!
//! Demonstrates **Tower Service layer integration** for authentication.
//!
//! ## Why This Approach
//!
//! **Ecosystem compatibility**: Leverages battle-tested `tower-http` middleware.
//! Can reuse existing Tower layers (CORS, Trace, Timeout, RateLimit) without modification.
//!
//! **Gradual migration**: If you have an existing Tower app, you can keep Tower auth
//! and gradually add Ranvier for business logic.
//!
//! **Team knowledge**: If your team already knows Tower, minimal learning curve.
//!
//! ## Trade-offs
//!
//! **Pros**:
//! - ✅ Reuse entire Tower ecosystem (CORS, Trace, Timeout, etc.)
//! - ✅ Team experience with Tower is directly applicable
//! - ✅ Battle-tested production middleware
//!
//! **Cons**:
//! - ❌ Tower layers are opaque in Ranvier Schematic (can't visualize auth flow)
//! - ❌ AuthContext stored in request extensions (not in Ranvier Bus)
//! - ❌ More boilerplate for custom layers (though high-level API helps)
//!
//! ## Run
//!
//! ```bash
//! cargo run -p auth-tower-integration
//! ```
//!
//! ## Compare with
//!
//! - `examples/auth-transition/` — Pure Ranvier approach (recommended)
//! - `docs/guides/auth-comparison.md` — Detailed comparison

mod auth;
mod tower_auth;

use ranvier_core::{Bus, Outcome};
use ranvier_macros::transition;
use ranvier_runtime::Axon;
use serde::{Deserialize, Serialize};
use std::env;
use tower_auth::jwt_auth_layer;

// ============================================================================
// Domain Types
// ============================================================================

/// HTTP response.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Response {
    status: u16,
    body: String,
}

/// Application-level errors.
#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
enum AppError {
    #[error("Internal server error: {0}")]
    Internal(String),
}

// ============================================================================
// Transitions
// ============================================================================

/// Protected handler — reads `AuthContext` from request extensions (not Bus).
///
/// Tower already validated the JWT and stored `AuthContext` in request extensions.
/// This transition extracts it and uses it for business logic.
#[transition]
async fn protected_handler(_input: (), _res: &(), bus: &mut Bus) -> Outcome<Response, AppError> {
    // In a real Tower+Ranvier integration, you'd extract AuthContext from request extensions.
    // For this demo, we simulate by reading from Bus (in production, you'd put it there
    // after extracting from request.extensions()).
    let auth = bus
        .read::<auth::AuthContext>()
        .expect("AuthContext should be in Bus");

    tracing::info!(
        user_id = %auth.user_id,
        "Protected handler executed (Tower verified token)"
    );

    let body = serde_json::json!({
        "message": format!("Hello, {}! (Verified by Tower)", auth.user_id),
        "user_id": auth.user_id,
        "roles": auth.roles,
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "verified_by": "Tower middleware",
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
                .unwrap_or_else(|_| "auth_tower_integration=info".into()),
        )
        .init();

    tracing::info!("Starting auth-tower-integration example (Tower + Ranvier)");

    let jwt_secret = env::var("JWT_SECRET").expect(
        "JWT_SECRET environment variable must be set. \
         Example: JWT_SECRET=your-secret-here cargo run",
    );

    // ── Tower Layer Setup ────────────────────────────────────────────────
    //
    // This is where Tower handles authentication:
    // 1. Extract Authorization header
    // 2. Validate JWT
    // 3. Store AuthContext in request extensions
    // 4. Return 401 if invalid
    //
    // Note: This auth layer is NOT visible in Ranvier Schematic.
    let _auth_layer = jwt_auth_layer(jwt_secret.clone());

    // ── Ranvier Pipeline Setup ───────────────────────────────────────────
    //
    // Ranvier handles business logic AFTER Tower validates the token.
    // Tower + Ranvier integration pattern:
    //
    // HTTP Request → Tower Auth Layer → Ranvier Handler
    //                (JWT validation)    (Business logic)
    //
    // The Ranvier pipeline assumes AuthContext is already validated.
    let ranvier_pipeline = Axon::simple::<AppError>("protected-handler")
        .then(protected_handler);

    if ranvier_pipeline.maybe_export_and_exit()? {
        return Ok(());
    }

    tracing::info!("Tower auth layer configured (JWT validation)");
    tracing::info!("Ranvier pipeline configured (business logic)");
    tracing::info!("\nIn production, you'd wrap this with Tower ServiceBuilder:");
    tracing::info!("  ServiceBuilder::new()");
    tracing::info!("    .layer(CorsLayer::permissive())");
    tracing::info!("    .layer(jwt_auth_layer(secret))");
    tracing::info!("    .service(ranvier_adapter)");

    // For demonstration, simulate Tower+Ranvier integration
    demo_execution(ranvier_pipeline, jwt_secret).await?;

    Ok(())
}

/// Demonstration of Tower + Ranvier integration.
///
/// In production:
/// 1. Tower middleware validates JWT → stores AuthContext in request.extensions()
/// 2. Adapter extracts AuthContext from extensions → puts in Bus
/// 3. Ranvier transitions read from Bus
async fn demo_execution(
    pipeline: Axon<(), Response, AppError, ()>,
    jwt_secret: String,
) -> anyhow::Result<()> {
    tracing::info!("\n=== Demo Execution ===\n");

    // Scenario 1: Valid admin token
    {
        tracing::info!("Scenario 1: Tower validates token, Ranvier handles request");

        // Simulate what Tower middleware does:
        let token = create_test_token("alice", vec!["admin".into(), "user".into()], &jwt_secret);
        let auth_ctx = auth::validate_jwt(&token, &jwt_secret)?;

        // In production, Tower puts this in request.extensions()
        // Then adapter extracts and puts in Bus
        let mut bus = Bus::new();
        bus.insert(auth_ctx);

        match pipeline.execute((), &(), &mut bus).await {
            Outcome::Next(response) => {
                tracing::info!("✅ Success: {}", response.body);
            }
            other => {
                tracing::error!("❌ Error: {:?}", other);
            }
        }
    }

    // Scenario 2: Tower would reject invalid token (never reaches Ranvier)
    {
        tracing::info!("\nScenario 2: Tower rejects invalid token (doesn't reach Ranvier)");
        tracing::info!("  In production, Tower returns 401 before calling Ranvier");
        tracing::info!("  Ranvier pipeline never executes");
    }

    Ok(())
}

/// Helper to create test JWT tokens.
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
