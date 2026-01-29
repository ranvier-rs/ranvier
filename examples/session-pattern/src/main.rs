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
use serde::{Deserialize, Serialize};

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
    type Error = anyhow::Error;

    async fn run(
        &self,
        req: String,
        bus: &mut Bus,
    ) -> anyhow::Result<Outcome<String, Self::Error>> {
        // Simulate extracting cookie from "Request" (here just the string input)
        // In real app: bus.req.headers().get("Cookie")...
        let sid = if req.contains("sid=") {
            req.split("sid=")
                .nth(1)
                .unwrap_or("")
                .trim_end_matches(')')
                .to_string()
        } else {
            "".to_string()
        };

        if !sid.is_empty() {
            if let Some(session) = SessionStore::load(&sid) {
                println!("[LoadSession] Session loaded for: {}", session.username);
                // CRITICAL: Explicitly write session to Bus
                bus.write(session);
            } else {
                println!("[LoadSession] Invalid Session ID");
            }
        } else {
            println!("[LoadSession] No Session ID found");
        }

        // Always continue. Session presence is checked later.
        Ok(Outcome::Next(req))
    }
}

/// Transition: Checks Authentication
#[derive(Clone)]
struct RequireAuth;

#[async_trait]
impl Transition<String, String> for RequireAuth {
    type Error = anyhow::Error;

    async fn run(
        &self,
        req: String,
        bus: &mut Bus,
    ) -> anyhow::Result<Outcome<String, Self::Error>> {
        // Check if session exists in Bus
        if bus.has::<UserSession>() {
            println!("[RequireAuth] Authorized.");
            Ok(Outcome::Next(req))
        } else {
            println!("[RequireAuth] Unauthorized! Branching to login.");
            // Branch to "login_flow" with reason
            Ok(Outcome::Branch(
                "login_flow".to_string(),
                Box::new("Authentication Required".to_string()),
            ))
        }
    }
}

/// Transition: Business Logic requiring Session
#[derive(Clone)]
struct UserProfile;

#[async_trait]
impl Transition<String, String> for UserProfile {
    type Error = anyhow::Error;

    async fn run(
        &self,
        _req: String,
        bus: &mut Bus,
    ) -> anyhow::Result<Outcome<String, Self::Error>> {
        // Safe unwrap because we are after RequireAuth
        // But idiomatic way is to use if let to be safe or map
        if let Some(session) = bus.read::<UserSession>() {
            let profile = format!("Profile: {} (Roles: {:?})", session.username, session.roles);
            Ok(Outcome::Next(profile))
        } else {
            // Should not happen if schematic is correct, but runtime safe
            Ok(Outcome::Fault(anyhow::anyhow!(
                "Session missing in UserProfile"
            )))
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

    let axon = Axon::start(req_valid, "SecureProfileFlow")
        .then(LoadSession)
        .then(RequireAuth)
        .then(UserProfile);

    // Schematic
    // println!("{}", serde_json::to_string_pretty(&axon.schematic)?);

    let mut bus = Bus::new(http::Request::new(()));
    let result = axon.execute(&mut bus).await?;

    match result {
        Outcome::Next(profile) => println!("Success: {}", profile),
        Outcome::Branch(route, _) => println!("Redirected: {}", route),
        Outcome::Fault(e) => println!("Error: {:?}", e),
        _ => {}
    }

    // Case 2: Invalid Session
    println!("\n--- Case 2: No/Invalid Session ---");
    let req_invalid = "GET /profile (No Cookie)".to_string();

    let axon2 = Axon::start(req_invalid, "SecureProfileFlow")
        .then(LoadSession)
        .then(RequireAuth)
        .then(UserProfile);

    let mut bus2 = Bus::new(http::Request::new(()));
    let result2 = axon2.execute(&mut bus2).await?;

    match result2 {
        Outcome::Next(profile) => println!("Success: {}", profile),
        Outcome::Branch(route, reason) => {
            if let Some(r) = reason.downcast_ref::<String>() {
                println!("Redirected to '{}': {}", route, r);
            }
        }
        Outcome::Fault(e) => println!("Error: {:?}", e),
        _ => {}
    }

    Ok(())
}
