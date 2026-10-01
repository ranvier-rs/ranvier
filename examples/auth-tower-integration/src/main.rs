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

mod adapter;
mod auth;
mod tower_auth;

#[cfg(test)]
use ranvier_core::Bus;
use ranvier_core::Outcome;
use ranvier_macros::transition;
use ranvier_runtime::Axon;
#[cfg(test)]
use ranvier_runtime::ExecutionReport;
use serde::{Deserialize, Serialize};
use std::env;
use tower::{Layer, Service};
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
    #[error("Authentication context is missing")]
    Unauthenticated,
    #[error("Internal server error: {0}")]
    Internal(String),
}

// ============================================================================
// Transitions
// ============================================================================

/// Protected handler reads identity transferred by the adapter to a fresh Bus.
#[transition]
async fn protected_handler(_input: (), _res: &(), bus: &mut Bus) -> Outcome<Response, AppError> {
    let Some(auth) = bus.read::<auth::AuthContext>() else {
        return Outcome::Fault(AppError::Unauthenticated);
    };

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

    let jwt_secret = env::var("JWT_SECRET")
        .map_err(|_| anyhow::anyhow!("JWT_SECRET environment variable must be set"))?;
    if jwt_secret.trim().is_empty() {
        anyhow::bail!("JWT_SECRET must not be empty");
    }

    let ranvier_pipeline = Axon::simple::<AppError>("protected-handler").then(protected_handler);

    if ranvier_pipeline.maybe_export_and_exit()? {
        return Ok(());
    }

    // Traverse the actual layer and adapter without starting an HTTP server.
    demo_execution(ranvier_pipeline, jwt_secret).await?;

    Ok(())
}

/// Traverse Tower validation, extensions-to-Bus handoff and Axon execution.
async fn demo_execution(
    pipeline: Axon<(), Response, AppError, ()>,
    jwt_secret: String,
) -> anyhow::Result<()> {
    let token = create_test_token("alice", vec!["admin".into(), "user".into()], &jwt_secret)?;
    let mut service = jwt_auth_layer(jwt_secret).layer(adapter::AxonService { pipeline });
    let mut request = http::Request::new(String::new());
    request.headers_mut().insert(
        http::header::AUTHORIZATION,
        format!("Bearer {token}").parse()?,
    );
    std::future::poll_fn(|cx| service.poll_ready(cx)).await?;
    let response = service.call(request).await?;
    if response.status() != http::StatusCode::OK {
        anyhow::bail!("Protected execution did not complete successfully");
    }
    tracing::info!("Authenticated request completed through Tower and Axon");
    std::future::poll_fn(|cx| service.poll_ready(cx)).await?;
    let denied = service.call(http::Request::new(String::new())).await?;
    if denied.status() != http::StatusCode::UNAUTHORIZED || denied.body() != "Unauthorized" {
        anyhow::bail!("Unauthenticated request was not safely rejected");
    }
    Ok(())
}

/// Helper to create test JWT tokens.
fn create_test_token(user_id: &str, roles: Vec<String>, secret: &str) -> anyhow::Result<String> {
    use jsonwebtoken::{EncodingKey, Header, encode};
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

    Ok(encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_identity_returns_fault_without_panicking() {
        let pipeline = Axon::simple::<AppError>("missing-identity").then(protected_handler);
        let mut bus = Bus::new();
        let report = pipeline.execute((), &(), &mut bus).await;
        assert!(
            !report
                .cleanup()
                .is_some_and(|cleanup| !cleanup.is_complete())
        );
        assert!(matches!(
            report,
            ExecutionReport::Unpersisted {
                result: Outcome::Fault(AppError::Unauthenticated),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn valid_identity_completes_local_demo() {
        let pipeline = Axon::simple::<AppError>("valid-identity").then(protected_handler);
        demo_execution(pipeline, "test-only-secret".to_owned())
            .await
            .unwrap();
    }
}
