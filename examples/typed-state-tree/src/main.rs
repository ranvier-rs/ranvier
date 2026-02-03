/*!
# Typed State Tree Example

This example demonstrates how to use Rust enums to represent a "Typed State Tree".
Instead of a simple linear flow, the Axon moves between explicit domain states.

## Key Concepts
1. **Explicit States**: Each stage in the process is represented by a variant in an enum.
2. **Type-Safe Transitions**: Transitions move from one state variant to another.
3. **Outcome Control**: The flow is controlled by returning `Outcome::Next`, `Outcome::Branch`, etc.
*/

use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_runtime::Axon;

// ============================================================================
// 1. Define the State Tree
// ============================================================================

/// Represents the possible states of our application flow.
#[derive(Debug, Clone)]
enum FlowState {
    /// Initial state: we have a raw request (e.g. URI)
    RequestReceived(String),
    /// State 2: Request has been authenticated, we have a User ID
    Authenticated { user_id: String, path: String },
    /// State 3: Content has been fetched from DB
    ContentLoaded { _user_id: String, data: String },
}

// ============================================================================
// 2. Define Transitions
// ============================================================================

/// Transition: RequestReceived -> Authenticated
#[derive(Clone)]
struct Authenticate;

#[async_trait]
impl Transition<FlowState, FlowState> for Authenticate {
    type Error = anyhow::Error;

    async fn run(&self, state: FlowState, _bus: &mut Bus) -> Outcome<FlowState, Self::Error> {
        if let FlowState::RequestReceived(uri) = state {
            println!("[Sync] Authenticating request for: {}", uri);

            // Simulation logic
            if uri == "/forbidden" {
                return Outcome::Fault(anyhow::anyhow!("Access Denied"));
            }

            Outcome::Next(FlowState::Authenticated {
                user_id: "user_123".to_string(),
                path: uri,
            })
        } else {
            Outcome::Fault(anyhow::anyhow!(
                "Unexpected State: Expected RequestReceived"
            ))
        }
    }
}

/// Transition: Authenticated -> ContentLoaded
#[derive(Clone)]
struct FetchContent;

#[async_trait]
impl Transition<FlowState, FlowState> for FetchContent {
    type Error = anyhow::Error;

    async fn run(&self, state: FlowState, _bus: &mut Bus) -> Outcome<FlowState, Self::Error> {
        if let FlowState::Authenticated { user_id, path } = state {
            println!("[Sync] Fetching content for user {} at {}", user_id, path);

            Outcome::Next(FlowState::ContentLoaded {
                _user_id: user_id,
                data: format!("Secret data for {}", path),
            })
        } else {
            Outcome::Fault(anyhow::anyhow!("Unexpected State: Expected Authenticated"))
        }
    }
}

// ============================================================================
// 3. Main - Wire the Tree
// ============================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Typed State Tree Demo ===\n");

    // Construct the Axon
    // Even though the input/output of the Axon is FlowState, the INTERNAL
    // transitions move the variant forward.
    let axon = Axon::<FlowState, FlowState, anyhow::Error>::start("SecureContentFlow")
        .then(Authenticate)
        .then(FetchContent);

    let mut bus = Bus::new();

    // Case 1: Valid Path
    println!("--- Case 1: Valid Path ---");
    let input1 = FlowState::RequestReceived("/dashboard".to_string());
    match axon.execute(input1, &mut bus).await {
        Outcome::Next(final_state) => {
            if let FlowState::ContentLoaded { data, .. } = final_state {
                println!("Success! Final Data: {}", data);
            }
        }
        Outcome::Fault(e) => println!("Error: {}", e),
        _ => {}
    }

    // Case 2: Forbidden Path
    println!("\n--- Case 2: Forbidden Path ---");
    let input2 = FlowState::RequestReceived("/forbidden".to_string());
    match axon.execute(input2, &mut bus).await {
        Outcome::Fault(e) => println!("Caught expected error: {}", e),
        other => println!("Unexpected result: {:?}", other),
    }

    Ok(())
}
