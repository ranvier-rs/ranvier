/*!
# Session Pattern Demo

This example demonstrates the "Session as Resource" pattern in Ranvier.

## Key Concepts
1.  **Session != Auth**: Session is just data; Auth is access control.
2.  **Explicit Loading**: `LoadSession` module loads session from "Cookie" (simulated) into Bus.
3.  **Explicit Check**: `RequireAuth` module checks for session presence in Bus.
4.  **Schematic Visibility**: The flow clearly shows: `LoadSession -> RequireAuth -> AppLogic`.

## Flow
1.  Start (Request with/without cookie)
2.  `LoadSession` transition (tries to load session, puts in Bus if found)
3.  `RequireAuth` transition (checks Bus, branches to "login" if missing)
4.  `UserProfile` transition (reads session from Bus)

*/

use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_runtime::Axon;
use serde::{Deserialize, Serialize};

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
// 1. Data Types
// ============================================================================

/// The Session Resource.
/// This matches the "Session as Resource" validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct UserSession {
    user_id: String,
    username: String,
    roles: Vec<String>,
}

/// Simulated Session Store (In-memory)
struct SessionStore;

impl SessionStore {
    // Simulate loading from a DB or Redis
    fn load(sid: &str) -> Option<UserSession> {
        if sid == "valid_sid_123" {
            Some(UserSession {
                user_id: "u_1".to_string(),
                username: "alice".to_string(),
                roles: vec!["user".to_string()],
            })
        } else {
            None
        }
    }
}

// ============================================================================
// 2. Transitions
// ============================================================================

/// Transition: Request -> Request (Side-effect: Loads Session into Bus)
#[derive(Clone)]
struct LoadSession;

#[async_trait]
impl Transition<String, String> for LoadSession {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        req: String,
        _resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        // A reused Bus must never retain a previous request's identity.
        if matches!(
            bus.get::<UserSession>(),
            Err(ranvier_core::BusAccessError::Unauthorized { .. })
        ) {
            return Outcome::Fault("session_context_access_denied".into());
        }
        bus.remove::<UserSession>();
        if bus.has::<UserSession>() {
            return Outcome::Fault("session_context_clear_failed".into());
        }
        // This is an explicit simulated envelope, not an HTTP cookie parser.
        let cookie = req
            .strip_prefix("GET /profile (Cookie: ")
            .and_then(|value| value.strip_suffix(')'));
        if let Some(cookie) = cookie.filter(|value| value.len() <= 4096) {
            let mut session_ids = cookie
                .split(';')
                .filter_map(|field| field.trim().strip_prefix("sid="));
            if let Some(sid) = session_ids.next() {
                if session_ids.next().is_none() {
                    if let Some(session) = SessionStore::load(sid) {
                        if bus.try_insert(session).is_err() {
                            return Outcome::Fault("session_context_write_denied".into());
                        }
                    }
                }
            }
        }

        // Always continue. Session presence is checked later.
        Outcome::Next(req)
    }
}

/// Transition: Checks Authentication
#[derive(Clone)]
struct RequireAuth;

#[async_trait]
impl Transition<String, String> for RequireAuth {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        req: String,
        _resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        // Check if session exists in Bus
        if bus.has::<UserSession>() {
            println!("[RequireAuth] Authorized.");
            Outcome::Next(req)
        } else {
            println!("[RequireAuth] Unauthorized! Branching to login.");
            // Branch to "login_flow" with reason
            Outcome::Branch(
                "login_flow".to_string(),
                Some(serde_json::json!("Authentication Required")),
            )
        }
    }
}

/// Transition: Business Logic requiring Session
#[derive(Clone)]
struct UserProfile;

#[async_trait]
impl Transition<String, String> for UserProfile {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        _req: String,
        _resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        // Safe unwrap because we are after RequireAuth
        // But idiomatic way is to use if let to be safe or map
        if let Ok(session) = bus.get_cloned::<UserSession>() {
            let profile = format!("Profile: {} (Roles: {:?})", session.username, session.roles);
            Outcome::Next(profile)
        } else {
            // Should not happen if schematic is correct, but runtime safe
            Outcome::Fault("Session missing in UserProfile".to_string())
        }
    }
}

// ============================================================================
// 3. Main
// ============================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Session Pattern Demo ===\n");

    // Case 1: Valid Session
    println!("--- Case 1: Valid Session ---");
    let req_valid = "GET /profile (Cookie: sid=valid_sid_123)".to_string();

    let axon = Axon::<String, String, String>::new("SecureProfileFlow")
        .then(LoadSession)
        .then(RequireAuth)
        .then(UserProfile);

    // Schematic
    // println!("{}", serde_json::to_string_pretty(&axon.schematic)?);

    let mut bus = Bus::new();
    let result = local_demo_outcome(axon.execute(req_valid, &(), &mut bus).await)?;

    match result {
        Outcome::Next(profile) if profile == "Profile: alice (Roles: [\"user\"])" => {
            println!("Valid session accepted")
        }
        _ => anyhow::bail!("Valid session scenario failed"),
    }

    // Case 2: Invalid Session
    println!("\n--- Case 2: No/Invalid Session ---");
    let req_invalid = "GET /profile (No Cookie)".to_string();

    let axon2 = Axon::<String, String, String>::new("SecureProfileFlow")
        .then(LoadSession)
        .then(RequireAuth)
        .then(UserProfile);

    let mut bus2 = Bus::new();
    let result2 = local_demo_outcome(axon2.execute(req_invalid, &(), &mut bus2).await)?;

    match result2 {
        Outcome::Branch(route, _) if route == "login_flow" => println!("Missing session rejected"),
        _ => anyhow::bail!("Missing session scenario failed"),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn invalid_requests_clear_prior_identity() {
        for request in [
            "GET /profile (No Cookie)",
            "GET /profile (Cookie: sid=bad)",
            "GET /profile (Cookie: sid=valid_sid_123; sid=bad)",
            "GET /profile (Cookie: xsid=valid_sid_123)",
            "GET /profile (Cookie: sid=valid_sid_123) trailing",
        ] {
            let mut bus = Bus::new();
            bus.insert(SessionStore::load("valid_sid_123").unwrap());
            let flow = Axon::<String, String, String>::new("session-test")
                .then(LoadSession)
                .then(RequireAuth)
                .then(UserProfile);
            let outcome =
                local_demo_outcome(flow.execute(request.into(), &(), &mut bus).await).unwrap();
            assert!(matches!(outcome, Outcome::Branch(route, _) if route == "login_flow"));
            assert!(!bus.has::<UserSession>());
        }
    }
    #[derive(Clone)]
    struct DeniedSessionLoader;
    #[async_trait]
    impl Transition<String, String> for DeniedSessionLoader {
        type Error = String;
        type Resources = ();
        fn bus_access_policy(&self) -> Option<ranvier_core::BusAccessPolicy> {
            Some(ranvier_core::BusAccessPolicy::deny_only(vec![
                ranvier_core::BusTypeRef::of::<UserSession>(),
            ]))
        }
        async fn run(&self, req: String, res: &(), bus: &mut Bus) -> Outcome<String, String> {
            LoadSession.run(req, res, bus).await
        }
    }
    #[tokio::test]
    async fn denied_session_context_stops_actual_profile_chain() {
        let mut bus = Bus::new();
        bus.insert(SessionStore::load("valid_sid_123").unwrap());
        let flow = Axon::<String, String, String>::new("denied-session-context")
            .then(DeniedSessionLoader)
            .then(RequireAuth)
            .then(UserProfile);
        let outcome = local_demo_outcome(
            flow.execute("GET /profile (No Cookie)".into(), &(), &mut bus)
                .await,
        )
        .unwrap();
        assert!(
            matches!(outcome, Outcome::Fault(error) if error == "session_context_access_denied")
        );
        assert_eq!(bus.get::<UserSession>().unwrap().user_id, "u_1");
    }

    #[tokio::test]
    async fn valid_flow_returns_report_checked_profile() {
        let flow = Axon::<String, String, String>::new("valid-session")
            .then(LoadSession)
            .then(RequireAuth)
            .then(UserProfile);
        let result = local_demo_outcome(
            flow.execute(
                "GET /profile (Cookie: sid=valid_sid_123)".into(),
                &(),
                &mut Bus::new(),
            )
            .await,
        )
        .unwrap();
        assert!(
            matches!(result, Outcome::Next(profile) if profile == "Profile: alice (Roles: [\"user\"])")
        );
    }
    #[tokio::test]
    async fn valid_session_and_missing_profile_capability() {
        let mut bus = Bus::new();
        let result = LoadSession
            .run(
                "GET /profile (Cookie: other=x; sid=valid_sid_123)".into(),
                &(),
                &mut bus,
            )
            .await;
        assert!(matches!(result, Outcome::Next(_)));
        assert_eq!(bus.get_cloned::<UserSession>().unwrap().user_id, "u_1");
        assert!(matches!(
            UserProfile.run(String::new(), &(), &mut Bus::new()).await,
            Outcome::Fault(_)
        ));
    }
}
