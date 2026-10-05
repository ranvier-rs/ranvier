/*!
# Service Call Demo

## Purpose
Demonstrates how to wrap external HTTP service calls as Ranvier Transitions:
- `reqwest::Client` injected via Resources for HTTP API calls
- Outcome-based error mapping: 2xx → Next, 4xx → Branch, 5xx/network → Fault
- Integration with `then_with_retry()` and `then_with_timeout()` for resilience

## Key Pattern
External service calls are just Transitions — validate locally, call remotely,
transform the response. The timeout drops the local wait; it does not prove remote cancellation.
This runnable example makes one read-only GET per invocation and does not retry.

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
use ranvier_runtime::Axon;
use serde::{Deserialize, Serialize};
use std::time::Duration;

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

impl ApiResources {
    fn new(base_url: String) -> anyhow::Result<Self> {
        let parsed = reqwest::Url::parse(&base_url)?;
        anyhow::ensure!(
            matches!(parsed.scheme(), "http" | "https")
                && parsed.host_str().is_some()
                && parsed.username().is_empty()
                && parsed.password().is_none()
                && parsed.query().is_none()
                && parsed.fragment().is_none(),
            "Invalid service endpoint"
        );
        Ok(Self {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(10))
                .build()?,
            base_url: base_url.trim_end_matches('/').to_owned(),
        })
    }
}

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
            Err(_) => {
                return Outcome::Fault("Service request unavailable".into());
            }
        };

        let status = response.status().as_u16();

        // Step 2: Map HTTP status to Outcome
        match status {
            200..=299 => {
                let mut response = response;
                const MAX_BYTES: usize = 65536;
                if response
                    .content_length()
                    .is_some_and(|length| length > MAX_BYTES as u64)
                {
                    return Outcome::Fault("Service response too large".into());
                }
                let mut body = Vec::new();
                loop {
                    let chunk = match response.chunk().await {
                        Ok(Some(chunk)) => chunk,
                        Ok(None) => break,
                        Err(_) => return Outcome::Fault("Service response unavailable".into()),
                    };
                    if chunk.len() > MAX_BYTES - body.len() {
                        return Outcome::Fault("Service response too large".into());
                    }
                    body.extend_from_slice(&chunk);
                }
                #[derive(Deserialize)]
                struct Company {
                    name: String,
                }
                #[derive(Deserialize)]
                struct ApiProfile {
                    id: u32,
                    name: String,
                    email: String,
                    company: Company,
                }
                let parsed = match serde_json::from_slice::<ApiProfile>(&body) {
                    Ok(parsed) => parsed,
                    Err(_) => return Outcome::Fault("Invalid service response".into()),
                };
                if parsed.id != input.user_id
                    || parsed.name.trim().is_empty()
                    || parsed.email.trim().is_empty()
                    || parsed.company.name.trim().is_empty()
                {
                    return Outcome::Fault("Invalid service response".into());
                }
                Outcome::Next(UserProfile {
                    id: parsed.id,
                    name: parsed.name,
                    email: parsed.email,
                    company: parsed.company.name,
                })
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
                // Unhandled status → Fault; this demo does not automatically retry.
                println!("  [FetchUserFromApi] {} Server Error", status);
                Outcome::Fault(format!("Unexpected service status: HTTP {}", status))
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
            source: "configured-http-api".to_string(),
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

    let resources = ApiResources::new(
        std::env::var("SERVICE_CALL_BASE_URL")
            .unwrap_or_else(|_| "https://jsonplaceholder.typicode.com".to_string()),
    )?;

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

        match local_demo_outcome(pipeline.execute(request, &resources, &mut bus).await)? {
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
            other => anyhow::bail!("Unexpected service control outcome: {other:?}"),
        }
    }

    println!();

    // ── Scenario 2: Client error → Branch ────────────────────────
    println!("--- Scenario 2: Client error (404) → Branch ---");
    println!("    Fetch user #999 → HTTP 404 → Branch(\"client_error\")\n");
    {
        let pipeline = Axon::<UserLookupRequest, UserLookupRequest, String, ApiResources>::new(
            "UserLookup404",
        )
        .then(ValidateRequest)
        .then(FetchUserFromApi)
        .then(EnrichProfile);

        let request = UserLookupRequest { user_id: 999 };
        let mut bus = Bus::new();

        match local_demo_outcome(pipeline.execute(request, &resources, &mut bus).await)? {
            Outcome::Next(profile) => {
                println!(
                    "  Result: {} (API returned 200 for unknown ID)",
                    profile.user.name
                );
            }
            Outcome::Fault(err) => println!("  Fault: {}", err),
            Outcome::Branch(id, payload) => {
                println!("  Branch(\"{}\"): {:?}", id, payload);
                println!(
                    "  → In production, route to a fallback handler or return a friendly error"
                );
            }
            other => anyhow::bail!("Unexpected service control outcome: {other:?}"),
        }
    }

    println!();

    // ── Scenario 3: With retry + timeout ─────────────────────────
    println!("--- Scenario 3: Read-only service call with timeout ---");
    println!("    Validate → Fetch with timeout(5s) → Enrich\n");
    {
        let pipeline = Axon::<UserLookupRequest, UserLookupRequest, String, ApiResources>::new(
            "ResilientLookup",
        )
        .then(ValidateRequest)
        .then_with_timeout(FetchUserFromApi, Duration::from_secs(5), || {
            "Local request wait timed out after 5s; remote cancellation is not confirmed"
                .to_string()
        })
        .then(EnrichProfile);

        let request = UserLookupRequest { user_id: 3 };
        let mut bus = Bus::new();

        match local_demo_outcome(pipeline.execute(request, &resources, &mut bus).await)? {
            Outcome::Next(profile) => {
                println!(
                    "\n  Result: {} <{}> (with timeout guard)",
                    profile.user.name, profile.user.email
                );
            }
            Outcome::Fault(err) => println!("\n  Fault: {}", err),
            other => anyhow::bail!("Unexpected service control outcome: {other:?}"),
        }
    }

    println!();

    // ── Scenario 4: Validation failure (local) ───────────────────
    println!("--- Scenario 4: Local validation failure ---");
    println!("    Validate user_id=0 → immediate Fault (no network call)\n");
    {
        let pipeline = Axon::<UserLookupRequest, UserLookupRequest, String, ApiResources>::new(
            "ValidationFail",
        )
        .then(ValidateRequest)
        .then(FetchUserFromApi)
        .then(EnrichProfile);

        let request = UserLookupRequest { user_id: 0 };
        let mut bus = Bus::new();

        match local_demo_outcome(pipeline.execute(request, &resources, &mut bus).await)? {
            Outcome::Fault(err) => {
                println!("  Fault: {} (caught before network call)", err);
            }
            other => anyhow::bail!("Unexpected service control outcome: {other:?}"),
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

#[cfg(test)]
mod report_tests {
    use super::*;
    use ranvier_runtime::{
        CompletionConfirmation, EffectBoundary, ExecutionReport, PersistenceOperation,
        SagaRollbackOutcome, SagaRollbackReport, SagaRollbackStep,
    };
    async fn fixture(
        status: u16,
        body: String,
        delay: Duration,
    ) -> (ApiResources, tokio::task::JoinHandle<String>) {
        fixture_with_headers(status, body, delay, String::new()).await
    }
    async fn fixture_with_headers(
        status: u16,
        body: String,
        delay: Duration,
        extra_headers: String,
    ) -> (ApiResources, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let resources =
            ApiResources::new(format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let handle = tokio::spawn(async move {
            let (mut socket, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                let n = tokio::time::timeout(Duration::from_secs(5), socket.read(&mut buffer))
                    .await
                    .unwrap()
                    .unwrap();
                assert!(n > 0, "client closed before request headers were complete");
                assert!(
                    request.len() + n <= 8192,
                    "request headers exceeded the 8 KiB fixture limit"
                );
                request.extend_from_slice(&buffer[..n]);
            }
            tokio::time::sleep(delay).await;
            let response = format!(
                "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n{extra_headers}\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await; // Timed-out client may have closed.
            String::from_utf8(request).unwrap()
        });
        (resources, handle)
    }

    fn pipeline() -> Axon<UserLookupRequest, EnrichedProfile, String, ApiResources> {
        Axon::<UserLookupRequest, UserLookupRequest, String, ApiResources>::new("http-fixture")
            .then(ValidateRequest)
            .then(FetchUserFromApi)
            .then(EnrichProfile)
    }

    #[tokio::test]
    async fn actual_http_status_and_response_contracts_are_enforced() {
        let valid = r#"{"id":7,"name":"Fixture","email":"fixture@example.test","company":{"name":"Local"}}"#;
        let cases = [
            (200, valid.to_owned(), "next"),
            (404, "private body".into(), "branch"),
            (503, "private body".into(), "fault"),
            (302, "redirect".into(), "fault"),
            (200, "{}".into(), "fault"),
            (200, "invalid-json".into(), "fault"),
            (200, valid.replace("\"id\":7", "\"id\":8"), "fault"),
            (200, "a".repeat(65537), "fault"),
        ];
        for (status, body, expected) in cases {
            let (resources, server) = fixture(status, body, Duration::ZERO).await;
            let outcome = local_demo_outcome(
                pipeline()
                    .execute(
                        UserLookupRequest { user_id: 7 },
                        &resources,
                        &mut Bus::new(),
                    )
                    .await,
            )
            .unwrap();
            match (expected, outcome) {
                ("next", Outcome::Next(profile)) => assert_eq!(profile.user.id, 7),
                ("branch", Outcome::Branch(id, payload)) => {
                    assert_eq!(id, "client_error");
                    assert_eq!(payload.unwrap()["status"], 404);
                }
                ("fault", Outcome::Fault(error)) => assert!(!error.contains("private body")),
                (_, other) => panic!("unexpected {other:?}"),
            }
            assert!(server.await.unwrap().starts_with("GET /users/7 HTTP/1.1"));
        }
    }

    #[tokio::test]
    async fn invalid_input_does_not_connect_and_timeout_is_only_local_wait() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let resources =
            ApiResources::new(format!("http://{}", listener.local_addr().unwrap())).unwrap();
        assert!(matches!(
            local_demo_outcome(
                pipeline()
                    .execute(
                        UserLookupRequest { user_id: 0 },
                        &resources,
                        &mut Bus::new()
                    )
                    .await
            )
            .unwrap(),
            Outcome::Fault(_)
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(10), listener.accept())
                .await
                .is_err()
        );
        let (resources, server) = fixture(200, "{}".into(), Duration::from_secs(1)).await;
        let axon =
            Axon::<UserLookupRequest, UserLookupRequest, String, ApiResources>::new("timeout")
                .then_with_timeout(FetchUserFromApi, Duration::from_millis(500), || {
                    "Local wait elapsed".to_owned()
                });
        assert!(
            matches!(local_demo_outcome(axon.execute(UserLookupRequest { user_id: 7 }, &resources, &mut Bus::new()).await).unwrap(), Outcome::Fault(ref error) if error == "Local wait elapsed")
        );
        assert!(server.await.unwrap().starts_with("GET /users/7 HTTP/1.1")); // Server observed the request.
    }

    #[tokio::test]
    async fn redirects_do_not_contact_a_second_origin() {
        let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let location = format!(
            "Location: http://{}/private\r\n",
            target.local_addr().unwrap()
        );
        let (resources, server) =
            fixture_with_headers(302, String::new(), Duration::ZERO, location).await;
        assert!(matches!(
            local_demo_outcome(
                pipeline()
                    .execute(
                        UserLookupRequest { user_id: 7 },
                        &resources,
                        &mut Bus::new()
                    )
                    .await
            )
            .unwrap(),
            Outcome::Fault(_)
        ));
        server.await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), target.accept())
                .await
                .is_err()
        );
    }

    #[test]
    fn endpoint_configuration_rejects_credentials_query_and_unsupported_schemes() {
        for value in [
            "file:///tmp/users",
            "https://user:secret@example.test",
            "https://example.test?secret=value",
            "https://example.test#fragment",
        ] {
            assert!(ApiResources::new(value.into()).is_err());
        }
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
