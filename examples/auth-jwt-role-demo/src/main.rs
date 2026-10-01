//! # Auth JWT Role Demo
//!
//! Demonstrates JWT authentication and role-based access control using
//! `ranvier_core::iam` and `Axon::with_iam()`.
//!
//! ## Run
//! ```bash
//! cargo run -p auth-jwt-role-demo
//! ```
//!
//! ## Key Concepts
//! - `IamVerifier` trait implementation for HS256 JWT tokens
//! - `IamPolicy::RequireRole` for role-based access control
//! - `Axon::with_iam(policy, verifier)` for automatic token verification
//! - `IamToken` Bus injection for token delivery
//! - `IamIdentity` read from Bus by downstream Transitions
//! - `Outcome::Emit("iam.*")` events on auth failures
//!
//! ## Prerequisites
//! - `hello-world` — basic Transition + Axon usage
//! - `guard-demo` — Guard node pipeline patterns
//!
//! ## Next Steps
//! - `multitenancy-demo` — tenant isolation with authenticated identity

mod auth;

use async_trait::async_trait;
use ranvier_core::iam::{IamIdentity, IamPolicy, IamToken};
use ranvier_core::prelude::*;
use ranvier_runtime::{Axon, ExecutionReport};
use serde::{Deserialize, Serialize};

// ============================================================================
// Domain Types
// ============================================================================

/// Dashboard data returned to authenticated admin users.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct DashboardData {
    user: String,
    roles: Vec<String>,
    stats: DashboardStats,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DashboardStats {
    active_users: u64,
    pending_tasks: u64,
}

/// Public profile data (no auth required).
#[derive(Clone, Debug, Serialize, Deserialize)]
struct PublicProfile {
    message: String,
}

// ============================================================================
// Transitions
// ============================================================================

/// Admin dashboard — reads `IamIdentity` from Bus (inserted by `with_iam`).
#[derive(Clone)]
struct AdminDashboard;

#[async_trait]
impl Transition<(), DashboardData> for AdminDashboard {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        _input: (),
        _resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<DashboardData, Self::Error> {
        let identity = match bus.get_cloned::<IamIdentity>() {
            Ok(identity) => identity,
            Err(_) => return Outcome::Fault("Authenticated identity is unavailable".into()),
        };

        Outcome::next(DashboardData {
            user: identity.subject.clone(),
            roles: identity.roles.clone(),
            stats: DashboardStats {
                active_users: 42,
                pending_tasks: 7,
            },
        })
    }
}

/// Public endpoint — no authentication needed.
#[derive(Clone)]
struct PublicEndpoint;

#[async_trait]
impl Transition<(), PublicProfile> for PublicEndpoint {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        _input: (),
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<PublicProfile, Self::Error> {
        Outcome::next(PublicProfile {
            message: "Welcome! This endpoint is public.".into(),
        })
    }
}

/// User profile — requires any valid identity (RequireIdentity policy).
#[derive(Clone)]
struct UserProfile;

#[async_trait]
impl Transition<(), serde_json::Value> for UserProfile {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        _input: (),
        _resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<serde_json::Value, Self::Error> {
        let identity = match bus.get_cloned::<IamIdentity>() {
            Ok(identity) => identity,
            Err(_) => return Outcome::Fault("Authenticated identity is unavailable".into()),
        };

        Outcome::next(serde_json::json!({
            "subject": identity.subject,
            "roles": identity.roles,
            "claims": identity.claims,
        }))
    }
}

// ============================================================================
// Main
// ============================================================================

fn local_demo_outcome<T, E>(
    report: ExecutionReport<Outcome<T, E>>,
) -> anyhow::Result<Outcome<T, E>> {
    if report
        .cleanup()
        .is_some_and(|cleanup| !cleanup.is_complete())
    {
        anyhow::bail!("Execution requires reconciliation before retry");
    }
    match report {
        ExecutionReport::Unpersisted { result, .. } => Ok(result),
        _ => anyhow::bail!("Execution requires reconciliation before retry"),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    println!("=== Auth JWT Role Demo ===\n");

    let verifier = auth::JwtVerifier::from_env().map_err(anyhow::Error::msg)?;

    // ── Build Circuits ───────────────────────────────────────────────────

    // Public circuit: no IAM policy
    let public_circuit = Axon::simple::<String>("public-endpoint").then(PublicEndpoint);

    // User circuit: requires any valid identity
    let user_circuit = Axon::simple::<String>("user-profile")
        .with_iam(IamPolicy::RequireIdentity, verifier.clone())
        .then(UserProfile);

    // Admin circuit: requires "admin" role
    let admin_circuit = Axon::simple::<String>("admin-dashboard")
        .with_iam(IamPolicy::RequireRole("admin".into()), verifier.clone())
        .then(AdminDashboard);

    // ── Issue Tokens ─────────────────────────────────────────────────────

    let admin_token = verifier
        .issue_token("alice", vec!["admin".into(), "user".into()])
        .map_err(|e| anyhow::anyhow!(e))?;
    let user_token = verifier
        .issue_token("bob", vec!["user".into()])
        .map_err(|e| anyhow::anyhow!(e))?;

    println!("Two demo tokens issued; token values are not logged.\n");

    // ── Demo 1: Public endpoint (no token needed) ────────────────────────

    println!("--- Demo 1: Public endpoint (no token) ---");
    {
        let mut bus = Bus::new();
        let result = local_demo_outcome(public_circuit.execute((), &(), &mut bus).await)?;
        match &result {
            Outcome::Next(profile) => println!("  {}\n", profile.message),
            _ => println!("  Unexpected execution outcome\n"),
        }
    }

    // ── Demo 2: Admin dashboard with admin token ─────────────────────────

    println!("--- Demo 2: Admin dashboard (alice, admin role) ---");
    {
        let mut bus = Bus::new();
        bus.insert(IamToken(admin_token.clone()));
        let result = local_demo_outcome(admin_circuit.execute((), &(), &mut bus).await)?;
        match &result {
            Outcome::Next(data) => {
                println!("  User: {}", data.user);
                println!("  Roles: {:?}", data.roles);
                println!("  Active Users: {}", data.stats.active_users);
                println!("  Pending Tasks: {}\n", data.stats.pending_tasks);
            }
            Outcome::Emit(event, _) => {
                println!("  [AUTH FAILED] Event: {}", event);
            }
            _ => println!("  Unexpected execution outcome\n"),
        }
    }

    // ── Demo 3: Admin dashboard with user-only token (insufficient role) ─

    println!("--- Demo 3: Admin dashboard (bob, user role only → denied) ---");
    {
        let mut bus = Bus::new();
        bus.insert(IamToken(user_token.clone()));
        let result = local_demo_outcome(admin_circuit.execute((), &(), &mut bus).await)?;
        match &result {
            Outcome::Emit(event, _) => {
                println!("  [DENIED] Event: {}", event);
            }
            _ => println!("  Unexpected execution outcome\n"),
        }
    }

    // ── Demo 4: User profile with valid user token ───────────────────────

    println!("--- Demo 4: User profile (bob, RequireIdentity) ---");
    {
        let mut bus = Bus::new();
        bus.insert(IamToken(user_token.clone()));
        let result = local_demo_outcome(user_circuit.execute((), &(), &mut bus).await)?;
        match &result {
            Outcome::Next(profile) => {
                println!("  Profile: {}\n", serde_json::to_string_pretty(profile)?);
            }
            _ => println!("  Unexpected execution outcome\n"),
        }
    }

    // ── Demo 5: Admin dashboard with no token (missing token) ────────────

    println!("--- Demo 5: Admin dashboard (no token → missing_token) ---");
    {
        let mut bus = Bus::new();
        let result = local_demo_outcome(admin_circuit.execute((), &(), &mut bus).await)?;
        match &result {
            Outcome::Emit(event, _) => {
                println!("  [MISSING] Event: {}", event);
                println!();
            }
            _ => println!("  Unexpected execution outcome\n"),
        }
    }

    // ── Demo 6: Admin dashboard with invalid token ───────────────────────

    println!("--- Demo 6: Admin dashboard (invalid token → verification_failed) ---");
    {
        let mut bus = Bus::new();
        bus.insert(IamToken("invalid.jwt.token".into()));
        let result = local_demo_outcome(admin_circuit.execute((), &(), &mut bus).await)?;
        match &result {
            Outcome::Emit(event, _) => {
                println!("  [INVALID] Event: {}", event);
                println!();
            }
            _ => println!("  Unexpected execution outcome\n"),
        }
    }

    println!("done");
    Ok(())
}
