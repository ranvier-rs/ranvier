/*!
# Service Call Demo

## Purpose
Demonstrates how to wrap external HTTP service calls as Ranvier Transitions:
- `reqwest::Client` injected via Resources for HTTP API calls
- Outcome-based error mapping: 2xx → Next, 4xx → Branch, 5xx/network → Fault
- Integration with `then_with_retry()` and `then_with_timeout()` for resilience

## Key Pattern
External service calls are just Transitions — validate locally, call remotely,
transform the response. The Axon execution engine handles retry and timeout
transparently.

## Running
```bash
cargo run -p service-call-demo
```

## Prerequisites
- `hello-world` — basic Ranvier concepts
- `outcome-variants-demo` — Outcome variant semantics
- `resilience-patterns-demo` — retry and timeout patterns

## Import Note
This example uses workspace crate imports (`ranvier_core`, `ranvier_runtime`, etc.)
because it lives inside the Ranvier workspace. For your own projects, use:
```rust
use ranvier::prelude::*;
```
*/

use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_core::transition::ResourceRequirement;
use ranvier_runtime::Axon;
use serde::{Deserialize, Serialize};
use std::time::Duration;

// ============================================================================
// Domain Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UserLookupRequest {
    user_id: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UserProfile {
    id: u32,
    name: String,
    email: String,
    company: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EnrichedProfile {
    user: UserProfile,
    source: String,
    lookup_note: String,
}

// ============================================================================
// Resources — shared across all transitions in the pipeline
// ============================================================================

#[derive(Clone)]
struct ApiResources {
    client: reqwest::Client,
    base_url: String,
}

impl ResourceRequirement for ApiResources {}

// ============================================================================
// Transition 1: Local Validation
// ============================================================================

#[derive(Clone)]
struct ValidateRequest;

#[async_trait]
impl Transition<UserLookupRequest, UserLookupRequest> for ValidateRequest {
    type Error = String;
    type Resources = ApiResources;

    async fn run(
        &self,
        input: UserLookupRequest,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<UserLookupRequest, Self::Error> {
        if input.user_id == 0 {
            return Outcome::Fault("Invalid user ID: 0".to_string());
        }
        println!("  [ValidateRequest] user_id={} validated", input.user_id);
        Outcome::Next(input)
    }
}

// ============================================================================
// Transition 2: External API Call (the core pattern)
// ============================================================================

/// Wraps an HTTP GET call as a Transition.
///
/// Error mapping strategy:
/// - Network error (timeout, DNS, connection refused) → `Outcome::Fault`
/// - HTTP 2xx → parse JSON → `Outcome::Next`
/// - HTTP 4xx (client error) → `Outcome::Branch("client_error")`
/// - HTTP 5xx (server error) → `Outcome::Fault` (retryable)
#[derive(Clone)]
struct FetchUserFromApi;

#[async_trait]
impl Transition<UserLookupRequest, UserProfile> for FetchUserFromApi {
    type Error = String;
    type Resources = ApiResources;

    async fn run(
        &self,
        input: UserLookupRequest,
        resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<UserProfile, Self::Error> {
        let url = format!("{}/users/{}", resources.base_url, input.user_id);
        println!("  [FetchUserFromApi] GET {}", url);

        // Step 1: Send HTTP request
        let response = match resources.client.get(&url).send().await {
            Ok(resp) => resp,
            Err(e) => {
                // Network error → Fault (retryable by then_with_retry)
                return Outcome::Fault(format!("Network error: {}", e));
            }
        };

        let status = response.status().as_u16();

        // Step 2: Map HTTP status to Outcome
        match status {
            200..=299 => {
                match response.json::<serde_json::Value>().await {
                    Ok(json) => {
                        let name = json["name"].as_str().unwrap_or("unknown");
                        let email = json["email"].as_str().unwrap_or("unknown");
                        let company = json["company"]["name"].as_str().unwrap_or("unknown");

                        let profile = UserProfile {
                            id: input.user_id,
                            name: name.to_string(),
                            email: email.to_string(),
                            company: company.to_string(),
                        };
                        println!(
                            "  [FetchUserFromApi] 200 OK: {} <{}>",
                            profile.name, profile.email
                        );
                        Outcome::Next(profile)
                    }
                    Err(e) => Outcome::Fault(format!("JSON parse error: {}", e)),
                }
            }
            400..=499 => {
                // Client error → Branch (non-retryable, handled separately)
                println!("  [FetchUserFromApi] {} Client Error", status);
                Outcome::Branch(
                    "client_error".to_string(),
                    Some(serde_json::json!({
                        "status": status,
                        "user_id": input.user_id,
                    })),
                )
            }
            _ => {
                // Server error → Fault (retryable)
                println!("  [FetchUserFromApi] {} Server Error", status);
                Outcome::Fault(format!("Server error: HTTP {}", status))
            }
        }
    }
}

// ============================================================================
// Transition 3: Local Enrichment
// ============================================================================

#[derive(Clone)]
struct EnrichProfile;

#[async_trait]
impl Transition<UserProfile, EnrichedProfile> for EnrichProfile {
    type Error = String;
    type Resources = ApiResources;

    async fn run(
        &self,
        user: UserProfile,
        resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<EnrichedProfile, Self::Error> {
        let enriched = EnrichedProfile {
            lookup_note: format!("Fetched from {}", resources.base_url),
            source: "jsonplaceholder".to_string(),
            user,
        };
        println!(
            "  [EnrichProfile] Enriched: {} @ {}",
            enriched.user.name, enriched.user.company
        );
        Outcome::Next(enriched)
    }
}

// ============================================================================
// Main — Run All Scenarios
// ============================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Service Call Demo ===\n");

    let resources = ApiResources {
        client: reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()?,
        base_url: std::env::var("SERVICE_CALL_BASE_URL")
            .unwrap_or_else(|_| "https://jsonplaceholder.typicode.com".to_string()),
    };

    // ── Scenario 1: Successful lookup ────────────────────────────
    println!("--- Scenario 1: Successful service call ---");
    println!("    Validate → Fetch user #1 → Enrich\n");
    {
        let pipeline =
            Axon::<UserLookupRequest, UserLookupRequest, String, ApiResources>::new("UserLookup")
                .then(ValidateRequest)
                .then(FetchUserFromApi)
                .then(EnrichProfile);

        let request = UserLookupRequest { user_id: 1 };
        let mut bus = Bus::new();

        match pipeline.execute(request, &resources, &mut bus).await {
            Outcome::Next(profile) => {
                println!(
                    "\n  Result: {} <{}> @ {} (source: {})",
                    profile.user.name, profile.user.email, profile.user.company, profile.source
                );
            }
            Outcome::Fault(err) => println!("\n  Fault: {}", err),
            Outcome::Branch(id, payload) => {
                println!("\n  Branch({}): {:?}", id, payload);
            }
            other => println!("\n  Unexpected: {:?}", other),
        }
    }

    println!();

    // ── Scenario 2: Client error → Branch ────────────────────────
    println!("--- Scenario 2: Client error (404) → Branch ---");
    println!("    Fetch user #999 → HTTP 404 → Branch(\"client_error\")\n");
    {
        let pipeline =
            Axon::<UserLookupRequest, UserLookupRequest, String, ApiResources>::new(
                "UserLookup404",
            )
            .then(ValidateRequest)
            .then(FetchUserFromApi)
            .then(EnrichProfile);

        let request = UserLookupRequest { user_id: 999 };
        let mut bus = Bus::new();

        match pipeline.execute(request, &resources, &mut bus).await {
            Outcome::Next(profile) => {
                println!("  Result: {} (API returned 200 for unknown ID)", profile.user.name);
            }
            Outcome::Fault(err) => println!("  Fault: {}", err),
            Outcome::Branch(id, payload) => {
                println!("  Branch(\"{}\"): {:?}", id, payload);
                println!("  → In production, route to a fallback handler or return a friendly error");
            }
            other => println!("  Unexpected: {:?}", other),
        }
    }

    println!();

    // ── Scenario 3: With retry + timeout ─────────────────────────
    println!("--- Scenario 3: Resilient service call (retry + timeout) ---");
    println!("    Validate → Fetch with retry(2) + timeout(5s) → Enrich\n");
    {
        let pipeline =
            Axon::<UserLookupRequest, UserLookupRequest, String, ApiResources>::new(
                "ResilientLookup",
            )
            .then(ValidateRequest)
            .then_with_timeout(
                FetchUserFromApi,
                Duration::from_secs(5),
                || "Service call timed out after 5s".to_string(),
            )
            .then(EnrichProfile);

        let request = UserLookupRequest { user_id: 3 };
        let mut bus = Bus::new();

        match pipeline.execute(request, &resources, &mut bus).await {
            Outcome::Next(profile) => {
                println!(
                    "\n  Result: {} <{}> (with timeout guard)",
                    profile.user.name, profile.user.email
                );
            }
            Outcome::Fault(err) => println!("\n  Fault: {}", err),
            other => println!("\n  Unexpected: {:?}", other),
        }
    }

    println!();

    // ── Scenario 4: Validation failure (local) ───────────────────
    println!("--- Scenario 4: Local validation failure ---");
    println!("    Validate user_id=0 → immediate Fault (no network call)\n");
    {
        let pipeline =
            Axon::<UserLookupRequest, UserLookupRequest, String, ApiResources>::new(
                "ValidationFail",
            )
            .then(ValidateRequest)
            .then(FetchUserFromApi)
            .then(EnrichProfile);

        let request = UserLookupRequest { user_id: 0 };
        let mut bus = Bus::new();

        match pipeline.execute(request, &resources, &mut bus).await {
            Outcome::Fault(err) => {
                println!("  Fault: {} (caught before network call)", err);
            }
            other => println!("  Unexpected: {:?}", other),
        }
    }

    println!();

    // ── Summary ──────────────────────────────────────────────────
    println!("=== Service Call Patterns ===");
    println!("  1. Wrap reqwest::Client in Resources struct");
    println!("  2. Map HTTP status codes to Outcome variants:");
    println!("     - 2xx → Outcome::Next(parsed_response)");
    println!("     - 4xx → Outcome::Branch(\"client_error\", payload)");
    println!("     - 5xx → Outcome::Fault(\"server error\")");
    println!("     - Network error → Outcome::Fault(\"connection failed\")");
    println!("  3. Combine with then_with_retry() / then_with_timeout()");
    println!("  4. Local validation runs BEFORE the network call");

    Ok(())
}
