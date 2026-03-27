/*!
# LLM Agent Pipeline Pattern

## Purpose
Demonstrates the **multi-stage pipeline pattern** using Ranvier's Axon.
A request flows through sequential transformation stages where each stage
enriches or transforms the data for the next.

## Pattern: Pipeline (Classify → Select → Execute → Merge)
Each stage has a distinct responsibility: classify the input, select a strategy,
execute the strategy, and merge/format the results. The Axon chain ensures
each stage's output is the next stage's input with full type safety.

## Applied Domain: AI Agent
User query → intent classification → tool selection → tool execution → response formatting.

## Key Concepts
- **Typed State Progression**: Each transition transforms the type (`Query → ClassifiedQuery → ToolResult → Response`)
- **Outcome::Branch**: Used for routing to different tool executors
- **Bus**: Carries classification metadata for downstream stages

## Running
```bash
cargo run -p llm-agent-pipeline
```

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

// ============================================================================
// Domain Types — Typed State Progression
// ============================================================================

/// Stage 0: Raw user query
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Query {
    user_id: String,
    text: String,
}

/// Stage 1: Query with classified intent
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClassifiedQuery {
    user_id: String,
    text: String,
    intent: Intent,
    confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum Intent {
    Calculator,
    Search,
    Summarize,
    Unknown,
}

/// Stage 2: Tool execution result
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ToolResult {
    user_id: String,
    original_query: String,
    intent: Intent,
    tool_output: String,
}

/// Stage 3: Final formatted response
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Response {
    user_id: String,
    answer: String,
    tool_used: String,
    confidence: f64,
}

// ============================================================================
// Stage 1: Intent Classification
// ============================================================================

#[derive(Clone)]
struct ClassifyIntent;

#[async_trait]
impl Transition<Query, ClassifiedQuery> for ClassifyIntent {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        query: Query,
        _resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<ClassifiedQuery, Self::Error> {
        println!("  [ClassifyIntent] Analyzing: \"{}\"", query.text);

        // Simulate intent classification (in production, this calls an LLM)
        let (intent, confidence) = if query.text.contains("calculate") || query.text.contains('+') || query.text.contains('*') {
            (Intent::Calculator, 0.95)
        } else if query.text.contains("search") || query.text.contains("find") {
            (Intent::Search, 0.88)
        } else if query.text.contains("summarize") || query.text.contains("summary") {
            (Intent::Summarize, 0.82)
        } else {
            (Intent::Unknown, 0.30)
        };

        println!("  [ClassifyIntent] Intent: {:?} (confidence: {:.0}%)", intent, confidence * 100.0);

        // Store confidence in Bus for downstream stages
        bus.insert(confidence);

        Outcome::Next(ClassifiedQuery {
            user_id: query.user_id,
            text: query.text,
            intent,
            confidence,
        })
    }
}

// ============================================================================
// Stage 2: Tool Selection & Execution
// ============================================================================

#[derive(Clone)]
struct ExecuteTool;

#[async_trait]
impl Transition<ClassifiedQuery, ToolResult> for ExecuteTool {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        query: ClassifiedQuery,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<ToolResult, Self::Error> {
        println!("  [ExecuteTool] Selecting tool for intent {:?}...", query.intent);

        let tool_output = match &query.intent {
            Intent::Calculator => {
                println!("  [ExecuteTool] Running calculator tool");
                "42 (simulated calculation result)".to_string()
            }
            Intent::Search => {
                println!("  [ExecuteTool] Running search tool");
                "Found 3 relevant documents (simulated)".to_string()
            }
            Intent::Summarize => {
                println!("  [ExecuteTool] Running summarizer tool");
                "The document discusses X, Y, and Z (simulated summary)".to_string()
            }
            Intent::Unknown => {
                return Outcome::Fault(format!(
                    "Cannot select tool: intent unknown for query \"{}\"",
                    query.text
                ));
            }
        };

        Outcome::Next(ToolResult {
            user_id: query.user_id,
            original_query: query.text,
            intent: query.intent,
            tool_output,
        })
    }
}

// ============================================================================
// Stage 3: Response Formatting
// ============================================================================

#[derive(Clone)]
struct FormatResponse;

#[async_trait]
impl Transition<ToolResult, Response> for FormatResponse {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        result: ToolResult,
        _resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<Response, Self::Error> {
        let tool_name = match &result.intent {
            Intent::Calculator => "calculator",
            Intent::Search => "search",
            Intent::Summarize => "summarizer",
            Intent::Unknown => "none",
        };

        let confidence = bus.get_cloned::<f64>().unwrap_or(0.0);

        let answer = format!(
            "Based on your query \"{}\", {} produced: {}",
            result.original_query, tool_name, result.tool_output
        );

        println!("  [FormatResponse] Composing final answer");

        Outcome::Next(Response {
            user_id: result.user_id,
            answer,
            tool_used: tool_name.to_string(),
            confidence,
        })
    }
}

// ============================================================================
// Main — Demonstrate Pipeline Pattern
// ============================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Multi-Stage Pipeline Pattern ===");
    println!("Pattern: Classify → Select → Execute → Format");
    println!("Domain example: AI Agent query processing\n");

    let pipeline = Axon::<Query, Query, String>::new("AgentPipeline")
        .then(ClassifyIntent)
        .then(ExecuteTool)
        .then(FormatResponse);

    if pipeline.maybe_export_and_exit()? {
        return Ok(());
    }

    // ── Scenario 1: Calculator intent ────────────────────────────
    println!("--- Scenario 1: Calculator query ---\n");
    {
        let query = Query {
            user_id: "user-1".to_string(),
            text: "calculate 6 * 7".to_string(),
        };
        let mut bus = Bus::new();

        match pipeline.execute(query, &(), &mut bus).await {
            Outcome::Next(resp) => {
                println!("\n  Response to {}: {}", resp.user_id, resp.answer);
                println!("  Tool: {}, Confidence: {:.0}%", resp.tool_used, resp.confidence * 100.0);
            }
            Outcome::Fault(err) => println!("\n  Error: {}", err),
            other => println!("\n  Unexpected: {:?}", other),
        }
    }

    // ── Scenario 2: Search intent ────────────────────────────────
    println!("\n--- Scenario 2: Search query ---\n");
    {
        let query = Query {
            user_id: "user-2".to_string(),
            text: "search for Rust async patterns".to_string(),
        };
        let mut bus = Bus::new();

        match pipeline.execute(query, &(), &mut bus).await {
            Outcome::Next(resp) => {
                println!("\n  Response to {}: {}", resp.user_id, resp.answer);
                println!("  Tool: {}, Confidence: {:.0}%", resp.tool_used, resp.confidence * 100.0);
            }
            Outcome::Fault(err) => println!("\n  Error: {}", err),
            other => println!("\n  Unexpected: {:?}", other),
        }
    }

    // ── Scenario 3: Unknown intent → Fault ───────────────────────
    println!("\n--- Scenario 3: Unknown intent (pipeline faults) ---\n");
    {
        let query = Query {
            user_id: "user-3".to_string(),
            text: "hello there".to_string(),
        };
        let mut bus = Bus::new();

        match pipeline.execute(query, &(), &mut bus).await {
            Outcome::Next(resp) => {
                println!("\n  Response: {}", resp.answer);
            }
            Outcome::Fault(err) => {
                println!("\n  Pipeline faulted: {}", err);
                println!("  (In production, fall back to general LLM response)");
            }
            other => println!("\n  Unexpected: {:?}", other),
        }
    }

    // ── Summary ──────────────────────────────────────────────────
    println!("\n=== Pipeline Pattern Summary ===");
    println!("  1. Each stage transforms Input → Output with a distinct type");
    println!("  2. Type safety ensures stages connect correctly at compile time");
    println!("  3. Outcome::Next carries transformed data to the next stage");
    println!("  4. Outcome::Fault short-circuits on unrecoverable errors");
    println!("  5. Bus carries cross-cutting data (e.g., confidence scores)");

    Ok(())
}
