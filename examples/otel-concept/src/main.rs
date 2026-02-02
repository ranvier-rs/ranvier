/*!
# OTLP Concept Example

This example demonstrates how to integrate OpenTelemetry-style tracing into Ranvier Axons.
It uses the `Traced` wrapper to automatically generate spans for Transitions.

## Key Concepts

1.  **ConnectionBus**: Wraps `Bus` to ensure connection ID is available.
2.  **TraceContext**: Carries Trace ID and Span ID (simulated).
3.  **Traced Wrapper**: `Traced::new(inner, "name")` decorates a Transition with logging/tracing logic.

*/

use async_trait::async_trait;
use ranvier_core::bus::{ConnectionBus, ConnectionId};
use ranvier_core::prelude::*;
use ranvier_core::telemetry::Traced;
use std::fmt::Debug;

// ============================================================================
// 1. Data Types
// ============================================================================

#[derive(Debug, Clone)]
struct HttpRequest {
    path: String,
    method: String,
}

#[derive(Debug, Clone)]
struct AuthUser {
    id: String,
    role: String,
}

#[derive(Debug, Clone)]
struct HttpResponse {
    status: u16,
    body: String,
}

// ============================================================================
// 2. Transitions
// ============================================================================

/// Transition: Parse Request -> AuthUser (Simulated Auth)
#[derive(Clone, Debug)]
struct Authenticate;

#[async_trait]
impl Transition<HttpRequest, AuthUser> for Authenticate {
    type Error = anyhow::Error;

    async fn run(&self, input: HttpRequest, _bus: &mut Bus) -> Outcome<AuthUser, Self::Error> {
        // Access Connection ID if available
        // Note: bus here is &mut Bus, but we know we started with ConnectionBus
        // If we need strict typing, we might use a trait or specialized read.

        // Let's simulate checking a header
        if input.path == "/login" {
            // Fail flow
            return Outcome::Fault(anyhow::anyhow!("Login not supported in this demo"));
        }

        Outcome::Next(AuthUser {
            id: "user_123".to_string(),
            role: "admin".to_string(),
        })
    }
}

/// Transition: Handle Logic -> HttpResponse
#[derive(Clone, Debug)]
struct HandleRequest;

#[async_trait]
impl Transition<AuthUser, HttpResponse> for HandleRequest {
    type Error = anyhow::Error;

    async fn run(&self, input: AuthUser, _bus: &mut Bus) -> Outcome<HttpResponse, Self::Error> {
        Outcome::Next(HttpResponse {
            status: 200,
            body: format!("Hello, {}! You are {}.", input.id, input.role),
        })
    }
}

// ============================================================================
// 3. Main Logic
// ============================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== OTLP Tracing Concept ===\n");

    // 1. Setup Request and Connection Context
    let req = http::Request::new(());
    let conn_id = ConnectionId::new();
    let bus = Bus::new();
    let mut conn_bus = ConnectionBus::from_bus(conn_id, bus);

    println!("[System] Connection ID: {:?}", conn_bus.connection_id());

    // 2. Define Axon with Tracing Wrappers
    // Provide a name for each span
    // 2. Define Axon with Tracing Wrappers
    // Provide a name for each span
    let axon = Axon::<HttpRequest, HttpRequest, anyhow::Error>::start("HttpTransaction")
        .then(Traced::new(Authenticate, "Authenticate"))
        .then(Traced::new(HandleRequest, "HandleRequest"));

    // 3. Execute
    let req_input = HttpRequest {
        path: "/dashboard".to_string(),
        method: "GET".to_string(),
    };

    // Note: ConnectionBus derefs to Bus, so we can pass it as &mut Bus
    match axon.execute(req_input, &mut conn_bus).await {
        Outcome::Next(res) => {
            println!("\n[Result] Status: {}, Body: {}", res.status, res.body);
        }
        Outcome::Fault(e) => {
            println!("\n[Result] Fault: {:?}", e);
        }
        _ => println!("\n[Result] Other outcome"),
    }

    Ok(())
}
