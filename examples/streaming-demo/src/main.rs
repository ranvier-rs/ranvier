//! # Streaming Demo — LLM Chat Streaming with SSE
//!
//! Demonstrates Ranvier's `StreamingTransition`, `#[streaming_transition]` macro,
//! `map_items()` per-item transform, and `post_sse_typed()` API.
//!
//! ## Pipeline
//!
//! ```text
//! ChatRequest → ClassifyIntent → synthesize_stream (streaming) → map_items(redact_pii) → SSE
//! ```
//!
//! ## Endpoints
//!
//! - `POST /api/chat/stream` — SSE streaming response with PII redaction
//! - `POST /api/chat`        — Non-streaming JSON response (for comparison)
//! - `GET /health`           — health endpoint
//! - `GET /ready` / `GET /live` — readiness/liveness probes
//!
//! ## Test
//!
//! ```bash
//! cargo run -p streaming-demo
//!
//! # SSE streaming (PII redacted)
//! curl -N -X POST http://localhost:3000/api/chat/stream \
//!   -H "Content-Type: application/json" \
//!   -d '{"message": "Hello, how are you?"}'
//!
//! # Non-streaming
//! curl -X POST http://localhost:3000/api/chat \
//!   -H "Content-Type: application/json" \
//!   -d '{"message": "Hello, how are you?"}'
//!
//! # Health / readiness
//! curl http://localhost:3000/health
//! curl http://localhost:3000/ready
//! curl http://localhost:3000/live
//! ```

use async_trait::async_trait;
use futures_core::Stream;
use ranvier_core::bus::Bus;
use ranvier_core::outcome::Outcome;
use ranvier_core::transition::Transition;
use ranvier_guard::prelude::*;
use ranvier_http::prelude::*;
use ranvier_macros::streaming_transition;
use ranvier_runtime::Axon;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ChatRequest {
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassifiedChat {
    pub message: String,
    pub intent: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatChunk {
    pub text: String,
    pub index: u32,
}

// ---------------------------------------------------------------------------
// Transitions
// ---------------------------------------------------------------------------

/// Step 1: Classify the user's intent.
#[derive(Clone)]
struct ClassifyIntent;

#[async_trait]
impl Transition<ChatRequest, ClassifiedChat> for ClassifyIntent {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: ChatRequest,
        _resources: &(),
        _bus: &mut Bus,
    ) -> Outcome<ClassifiedChat, String> {
        let intent = if input.message.to_lowercase().contains("code") {
            "code_generation"
        } else {
            "general_chat"
        };

        tracing::info!(intent = %intent, "Classified intent");

        Outcome::Next(ClassifiedChat {
            message: input.message,
            intent: intent.to_string(),
        })
    }

    fn label(&self) -> String {
        "ClassifyIntent".to_string()
    }
}

/// Step 2 (streaming): Simulate LLM token streaming using `#[streaming_transition]` macro.
///
/// This replaces the manual `impl StreamingTransition` with ~15 lines less boilerplate.
#[streaming_transition]
async fn synthesize_stream(
    input: ClassifiedChat,
) -> Result<impl Stream<Item = ChatChunk> + Send, String> {
    let tokens = match input.intent.as_str() {
        "code_generation" => vec![
            "Here's",
            " a",
            " simple",
            " example:\n",
            "```rust\n",
            "fn ",
            "hello",
            "() ",
            "{\n",
            "    ",
            "println!",
            "(\"Hello!\");\n",
            "}\n",
            "```",
        ],
        _ => vec![
            "Hello", "!", " I'm", " doing", " great,", " thank", " you", " for", " asking.",
            " How", " can", " I", " help", " you", " today?",
        ],
    };

    let stream = async_stream::stream! {
        for (i, token) in tokens.into_iter().enumerate() {
            tokio::time::sleep(Duration::from_millis(50)).await;
            yield ChatChunk {
                text: token.to_string(),
                index: i as u32,
            };
        }
    };

    Ok(stream)
}

/// PII redaction filter — replaces email-like patterns in stream chunks.
fn redact_pii(mut chunk: ChatChunk) -> ChatChunk {
    // Simple PII filter: redact anything that looks like an email
    if chunk.text.contains('@') {
        chunk.text = "[REDACTED]".to_string();
    }
    chunk
}

/// Step 2 (non-streaming): Generate complete response as JSON Value.
#[derive(Clone)]
struct SynthesizeBatch;

#[async_trait]
impl Transition<ClassifiedChat, serde_json::Value> for SynthesizeBatch {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: ClassifiedChat,
        _resources: &(),
        _bus: &mut Bus,
    ) -> Outcome<serde_json::Value, String> {
        tokio::time::sleep(Duration::from_millis(200)).await;

        let reply = match input.intent.as_str() {
            "code_generation" => {
                "Here's a simple example:\n```rust\nfn hello() {\n    println!(\"Hello!\");\n}\n```"
            }
            _ => "Hello! I'm doing great, thank you for asking. How can I help you today?",
        };

        Outcome::Next(serde_json::json!({ "reply": reply }))
    }

    fn label(&self) -> String {
        "SynthesizeBatch".to_string()
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter("streaming_demo=info,ranvier_http=info")
        .init();

    let bind_addr =
        std::env::var("STREAMING_DEMO_ADDR").unwrap_or_else(|_| "127.0.0.1:3000".to_string());

    // Streaming pipeline with macro + map_items PII filter:
    // ChatRequest → ClassifyIntent → synthesize_stream → map_items(redact_pii) → SSE
    let streaming_pipeline = Axon::typed::<ChatRequest, String>("chat-stream")
        .then(ClassifyIntent)
        .then_stream(synthesize_stream)
        .map_items(redact_pii);

    // Non-streaming pipeline: ChatRequest → ClassifyIntent → SynthesizeBatch → JSON
    let batch_pipeline = Axon::typed::<ChatRequest, String>("chat-batch")
        .then(ClassifyIntent)
        .then(SynthesizeBatch);

    tracing::info!(bind_addr = %bind_addr, "Starting streaming-demo");

    Ranvier::http()
        .bind(&bind_addr)
        .graceful_shutdown(Duration::from_secs(5))
        .guard(CorsGuard::<()>::permissive())
        .guard(AccessLogGuard::<()>::new())
        .guard(RequestIdGuard::<()>::new())
        .post_sse_typed::<ChatRequest, _, _>("/api/chat/stream", streaming_pipeline)
        .post_typed::<ChatRequest, _, _>("/api/chat", batch_pipeline)
        .health_endpoint("/health")
        .readiness_liveness_default()
        .run(())
        .await?;

    Ok(())
}
