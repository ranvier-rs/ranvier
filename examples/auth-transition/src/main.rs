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

use auth::{AuthContext, AuthError, validate_jwt};
use ranvier_core::{Bus, Outcome};
use ranvier_macros::transition;
use ranvier_runtime::Axon;
use serde::{Deserialize, Serialize};
use std::env;

struct AuthResources {
    jwt_secret: String,
}
#[cfg(test)]
struct HandlerVisited;
impl AuthResources {
    fn new(jwt_secret: String) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !jwt_secret.trim().is_empty(),
            "JWT_SECRET must be explicitly configured"
        );
        Ok(Self { jwt_secret })
    }
}

// Explicit local-demo policy: neither persisted nor incomplete cleanup results
// may be converted into ordinary domain success or automatic retry permission.
fn local_demo_outcome<T, E>(
    report: ranvier_runtime::ExecutionReport<Outcome<T, E>>,
) -> anyhow::Result<Outcome<T, E>> {
    if report
        .cleanup()
        .is_some_and(|cleanup| !cleanup.is_complete())
    {
        anyhow::bail!("Execution requires reconciliation before retry");
    }
    match report {
        ranvier_runtime::ExecutionReport::Unpersisted { result, .. } => Ok(result),
        _ => anyhow::bail!("Execution requires reconciliation before retry"),
    }
}

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
    Auth(String), // Can't use #[from] with serde

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
async fn authenticate(
    _input: (),
    _res: &AuthResources,
    bus: &mut Bus,
) -> Outcome<AuthContext, AppError> {
    bus.remove::<AuthContext>(); // Do not retain identity from a failed prior invocation.
    // In a real HTTP scenario, token would be extracted from request and put in Bus.
    // For this demo, we'll expect it to already be in Bus as a String.
    let token = match bus.get_cloned::<String>() {
        Ok(t) => t,
        Err(_) => return Outcome::Fault(AppError::Auth(AuthError::MissingHeader.to_string())),
    };

    // Validate JWT
    let auth_ctx = match validate_jwt(&token, &_res.jwt_secret) {
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
async fn authorize(
    _input: AuthContext,
    _res: &AuthResources,
    bus: &mut Bus,
) -> Outcome<(), AppError> {
    let auth = match bus.get_cloned::<AuthContext>() {
        Ok(auth) => auth,
        Err(_) => return Outcome::Fault(AppError::Auth("Authentication required".into())),
    };

    let required_role = "admin";

    if !auth.roles.contains(&required_role.to_string()) {
        tracing::warn!(
            user_id = %auth.user_id,
            required_role = %required_role,
            actual_roles = ?auth.roles,
            "Authorization failed: missing required role"
        );
        return Outcome::Fault(AppError::Auth(
            AuthError::Unauthorized(required_role.into()).to_string(),
        ));
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
async fn protected_handler(
    _input: (),
    _res: &AuthResources,
    bus: &mut Bus,
) -> Outcome<Response, AppError> {
    let auth = match bus.get_cloned::<AuthContext>() {
        Ok(auth) => auth,
        Err(_) => return Outcome::Fault(AppError::Auth("Authentication required".into())),
    };

    tracing::info!(
        user_id = %auth.user_id,
        "Protected handler executed for authenticated user"
    );

    #[cfg(test)]
    bus.insert(HandlerVisited);
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
        .try_init()
        .ok();

    tracing::info!("Starting auth-transition example (Ranvier Way)");

    // Build the authentication pipeline
    // authenticate → authorize → protected_handler
    //
    // Key insight: AuthContext flows through Bus.
    // - `authenticate` validates JWT, stores `AuthContext` in Bus
    // - `authorize` reads `AuthContext` from Bus, checks role
    // - `protected_handler` reads `AuthContext` from Bus, generates response
    let auth_pipeline = Axon::<(), (), AppError, AuthResources>::new("auth-pipeline")
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
    let resources = AuthResources::new(env::var("JWT_SECRET")?)?;
    demo_execution(auth_pipeline, &resources).await?;

    Ok(())
}

/// Demonstration of pipeline execution with various scenarios.
async fn demo_execution(
    pipeline: Axon<(), Response, AppError, AuthResources>,
    resources: &AuthResources,
) -> anyhow::Result<()> {
    tracing::info!("\n=== Demo Execution ===\n");

    // Scenario 1: Valid admin token
    {
        tracing::info!("Scenario 1: Valid admin token");
        let jwt_secret = &resources.jwt_secret;

        // Create test token (in real app, this comes from login endpoint)
        let token = create_test_token("alice", vec!["admin".into(), "user".into()], &jwt_secret)?;

        let mut bus = Bus::new();
        bus.insert(token); // Put token in Bus (simulating HTTP middleware)

        match local_demo_outcome(pipeline.execute((), resources, &mut bus).await)? {
            Outcome::Next(response) => {
                tracing::info!("✅ Success: {}", response.body);
            }
            other => {
                anyhow::bail!("Expected authenticated success, got {other:?}");
            }
        }
    }

    // Scenario 2: Valid token but missing "admin" role
    {
        tracing::info!("\nScenario 2: Valid token, no admin role");
        let jwt_secret = &resources.jwt_secret;

        let token = create_test_token("bob", vec!["user".into()], &jwt_secret)?;

        let mut bus = Bus::new();
        bus.insert(token);

        match local_demo_outcome(pipeline.execute((), resources, &mut bus).await)? {
            Outcome::Next(_) => {
                anyhow::bail!("Authentication unexpectedly succeeded");
            }
            Outcome::Fault(AppError::Auth(_)) => {
                tracing::info!("Expected authentication rejection")
            }
            _ => anyhow::bail!("Unexpected authentication control outcome"),
        }
    }

    // Scenario 3: Missing token
    {
        tracing::info!("\nScenario 3: Missing token");
        let mut bus = Bus::new(); // Empty bus

        match local_demo_outcome(pipeline.execute((), resources, &mut bus).await)? {
            Outcome::Next(_) => {
                anyhow::bail!("Authentication unexpectedly succeeded");
            }
            Outcome::Fault(AppError::Auth(_)) => {
                tracing::info!("Expected authentication rejection")
            }
            _ => anyhow::bail!("Unexpected authentication control outcome"),
        }
    }

    // Scenario 4: Invalid token
    {
        tracing::info!("\nScenario 4: Invalid token");
        let mut bus = Bus::new();
        bus.insert("invalid-token".to_string());

        match local_demo_outcome(pipeline.execute((), resources, &mut bus).await)? {
            Outcome::Next(_) => {
                anyhow::bail!("Authentication unexpectedly succeeded");
            }
            Outcome::Fault(AppError::Auth(_)) => {
                tracing::info!("Expected authentication rejection")
            }
            _ => anyhow::bail!("Unexpected authentication control outcome"),
        }
    }

    Ok(())
}

/// Helper to create test JWT tokens (for demonstration).
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
mod report_tests {
    use super::*;
    use ranvier_core::Transition;
    use ranvier_runtime::{
        CompletionConfirmation, EffectBoundary, ExecutionReport, PersistenceOperation,
        SagaRollbackOutcome, SagaRollbackReport, SagaRollbackStep,
    };
    fn pipeline() -> Axon<(), Response, AppError, AuthResources> {
        Axon::<(), (), AppError, AuthResources>::new("auth-test")
            .then(authenticate)
            .then(authorize)
            .then(protected_handler)
    }

    #[tokio::test]
    async fn authentication_and_role_failures_never_visit_protected_handler() {
        let resources = AuthResources::new("explicit-local-test-secret".into()).unwrap();
        let cases = [
            (
                Some(
                    create_test_token("alice", vec!["admin".into()], &resources.jwt_secret)
                        .unwrap(),
                ),
                true,
            ),
            (
                Some(create_test_token("bob", vec!["user".into()], &resources.jwt_secret).unwrap()),
                false,
            ),
            (
                Some(create_test_token(" ", vec!["admin".into()], &resources.jwt_secret).unwrap()),
                false,
            ),
            (Some("invalid".into()), false),
            (None, false),
        ];
        for (token, allowed) in cases {
            let mut bus = Bus::new();
            bus.insert(AuthContext {
                user_id: "forged".into(),
                roles: vec!["admin".into()],
            });
            if let Some(token) = token {
                bus.insert(token);
            }
            let outcome =
                local_demo_outcome(pipeline().execute((), &resources, &mut bus).await).unwrap();
            assert_eq!(matches!(outcome, Outcome::Next(_)), allowed);
            assert_eq!(bus.read::<HandlerVisited>().is_some(), allowed);
            if allowed {
                assert_eq!(bus.read::<AuthContext>().unwrap().user_id, "alice");
            }
        }
        assert!(AuthResources::new(" ".into()).is_err());
        assert!(matches!(
            authorize
                .run(
                    AuthContext {
                        user_id: "alice".into(),
                        roles: vec!["admin".into()]
                    },
                    &resources,
                    &mut Bus::new()
                )
                .await,
            Outcome::Fault(_)
        ));
        assert!(matches!(
            protected_handler.run((), &resources, &mut Bus::new()).await,
            Outcome::Fault(_)
        ));
    }

    #[tokio::test]
    async fn declared_demo_runs_with_explicit_resources() {
        let resources = AuthResources::new("explicit-local-test-secret".into()).unwrap();
        demo_execution(pipeline(), &resources).await.unwrap();
    }

    #[test]
    fn local_report_policy_preserves_faults_and_rejects_uncertain_or_incomplete_execution() {
        let next = || Outcome::<u32, String>::Next(7);
        let reports = [
            ExecutionReport::NotRequested {
                result: next(),
                cleanup: None,
            },
            ExecutionReport::Recorded {
                result: next(),
                completion: CompletionConfirmation::Confirmed,
                cleanup: None,
            },
            ExecutionReport::Recorded {
                result: next(),
                completion: CompletionConfirmation::NotRequested,
                cleanup: None,
            },
            ExecutionReport::Unconfirmed {
                result: next(),
                operation: PersistenceOperation::Result,
                effect: EffectBoundary::MayHaveRun,
                cleanup: None,
            },
            ExecutionReport::BlockedBeforeEffect {
                operation: PersistenceOperation::Intent,
                effect: EffectBoundary::NotInvoked,
                cleanup: None,
            },
            ExecutionReport::BlockedBeforeEffect {
                operation: PersistenceOperation::Intent,
                effect: EffectBoundary::MayHaveRun,
                cleanup: None,
            },
        ];
        for report in reports {
            assert_eq!(
                local_demo_outcome(report).unwrap_err().to_string(),
                "Execution requires reconciliation before retry"
            );
        }
        for (finished, outcome) in [
            (false, SagaRollbackOutcome::Completed),
            (true, SagaRollbackOutcome::Fault),
            (true, SagaRollbackOutcome::InProgress),
        ] {
            let report = ExecutionReport::Unpersisted {
                result: next(),
                cleanup: Some(SagaRollbackReport {
                    trace_id: "private-trace".into(),
                    finished,
                    steps: vec![SagaRollbackStep {
                        node_id: "private-node".into(),
                        outcome,
                    }],
                }),
            };
            assert_eq!(
                local_demo_outcome(report).unwrap_err().to_string(),
                "Execution requires reconciliation before retry"
            );
        }
        assert!(matches!(
            local_demo_outcome(ExecutionReport::Unpersisted {
                result: next(),
                cleanup: None
            })
            .unwrap(),
            Outcome::Next(7)
        ));
        assert!(
            matches!(local_demo_outcome(ExecutionReport::Unpersisted { result: Outcome::<u32, String>::Fault("expected-domain-failure".into()), cleanup: None }).unwrap(), Outcome::Fault(error) if error == "expected-domain-failure")
        );
    }
}
