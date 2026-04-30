/*!
# Outcome Patterns Demo (v0.42–v0.43)

Demonstrates **Outcome ergonomic APIs** introduced in v0.42–v0.43.
Each transition showcases a specific pattern with before/after comparison.

## Featured APIs

- **`try_outcome!`**: Ergonomic `Result → Outcome::Fault` conversion (2 forms)
- **`Outcome::from_result()`**: `Result<T, E> → Outcome<T, String>`
- **`Outcome::and_then()`**: Monadic chain composition
- **`Outcome::map_fault()`**: Error type transformation
- **`Outcome::map()`**: Value transformation
- **`Outcome::unwrap_or()`**: Default value extraction
- **`Bus::get_cloned()`**: Concise resource extraction
- **`json_outcome()`**: Serialize any value to JSON Outcome
- **`Axon::execute_simple()`**: Execute without resources

## Running

```bash
cargo run -p outcome-patterns
# Then: curl http://localhost:3200/api/try-outcome
#        curl http://localhost:3200/api/combinators
#        curl http://localhost:3200/api/get-cloned
#        curl http://localhost:3200/api/json-outcome
#        curl http://localhost:3200/api/execute-simple
```
*/

use async_trait::async_trait;
use ranvier_core::{prelude::*, try_outcome};
use ranvier_guard::prelude::*;
use ranvier_http::prelude::*;
use ranvier_runtime::Axon;
use serde::{Deserialize, Serialize};

// ─── Shared Config (injected via Bus) ───────────────────────────

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub app_name: String,
    pub version: String,
    pub max_retries: u32,
}

// ─── Demo 1: try_outcome! ───────────────────────────────────────

/// Shows both forms of `try_outcome!`:
/// - `try_outcome!(expr)` — converts Err to Fault(e.to_string())
/// - `try_outcome!(expr, "context")` — converts Err to Fault("context: error")
#[derive(Clone, Copy)]
struct TryOutcomeDemo;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ParsedData {
    number: i64,
    json_value: serde_json::Value,
    message: String,
}

#[async_trait]
impl Transition<(), ParsedData> for TryOutcomeDemo {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        _input: (),
        _res: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<ParsedData, Self::Error> {
        // Form 1: try_outcome!(expr) — bare conversion
        let number = try_outcome!("42".parse::<i64>());

        // Form 2: try_outcome!(expr, "context") — with context message
        let json_value = try_outcome!(
            serde_json::from_str::<serde_json::Value>(r#"{"key": "value"}"#),
            "JSON parse failed"
        );

        Outcome::Next(ParsedData {
            number,
            json_value,
            message: "Both try_outcome! forms succeeded".into(),
        })
    }
}

// ─── Demo 2: Outcome Combinators ────────────────────────────────

/// Shows `from_result`, `and_then`, `map_fault`, `map`, `unwrap_or`.
#[derive(Clone, Copy)]
struct CombinatorDemo;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CombinatorResult {
    from_result_demo: String,
    and_then_demo: String,
    map_fault_demo: String,
    map_demo: String,
    unwrap_or_demo: i64,
}

#[async_trait]
impl Transition<(), CombinatorResult> for CombinatorDemo {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        _input: (),
        _res: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<CombinatorResult, Self::Error> {
        // from_result: Result<T, E: Display> → Outcome<T, String>
        let parsed: Outcome<i64, String> = Outcome::from_result("100".parse::<i64>());
        let from_result_demo = format!("from_result(Ok(100)) = {:?}", parsed);

        // and_then: chain Outcome-returning closures
        let chained = Outcome::<i64, String>::Next(10)
            .and_then(|n| Outcome::Next(n * 2))
            .and_then(|n| Outcome::Next(format!("result = {n}")));
        let and_then_demo = format!("10 → ×2 → format = {:?}", chained);

        // map_fault: transform error type
        let fault = Outcome::<(), i32>::Fault(404).map_fault(|code| format!("HTTP {code}"));
        let map_fault_demo = format!("Fault(404) → map_fault = {:?}", fault);

        // map: transform success value
        let mapped = Outcome::<i64, String>::Next(5).map(|n| n * n);
        let map_demo = format!("Next(5) → map(n*n) = {:?}", mapped);

        // unwrap_or: extract value with default
        let fault_val: Outcome<i64, String> = Outcome::Fault("error".into());
        let unwrap_or_demo = fault_val.unwrap_or(0);

        Outcome::Next(CombinatorResult {
            from_result_demo,
            and_then_demo,
            map_fault_demo,
            map_demo,
            unwrap_or_demo,
        })
    }
}

// ─── Demo 3: Bus::get_cloned ────────────────────────────────────

/// Shows `Bus::get_cloned::<T>()` vs the old `bus.read::<T>().cloned()` pattern.
#[derive(Clone, Copy)]
struct GetClonedDemo;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConfigInfo {
    app_name: String,
    version: String,
    max_retries: u32,
    pattern: String,
}

#[async_trait]
impl Transition<(), ConfigInfo> for GetClonedDemo {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        _input: (),
        _res: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<ConfigInfo, Self::Error> {
        // v0.43: Bus::get_cloned() — one call, owned value
        let config = try_outcome!(bus.get_cloned::<AppConfig>(), "AppConfig not in Bus");

        // Before v0.43 you had to write:
        //   let config = bus.read::<AppConfig>()
        //       .cloned()
        //       .ok_or_else(|| "AppConfig not in Bus".to_string())?;
        // or use manual pattern matching.

        Outcome::Next(ConfigInfo {
            app_name: config.app_name,
            version: config.version,
            max_retries: config.max_retries,
            pattern: "Bus::get_cloned::<AppConfig>()".into(),
        })
    }
}

// ─── Demo 4: json_outcome() ────────────────────────────────────

/// Shows `json_outcome()` helper for manual JSON Outcome when needed.
#[derive(Clone, Copy)]
struct JsonOutcomeDemo;

#[async_trait]
impl Transition<(), String> for JsonOutcomeDemo {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        _input: (),
        _res: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        #[derive(Serialize)]
        struct Report {
            title: String,
            items: Vec<String>,
            count: usize,
        }

        let report = Report {
            title: "json_outcome() demo".into(),
            items: vec!["from_result".into(), "and_then".into(), "map_fault".into()],
            count: 3,
        };

        // json_outcome() serializes any Serialize value into Outcome<String, String>
        // Useful when a route uses `.get()` (not `get_json_out`) and you need JSON manually
        json_outcome(&report)
    }
}

// ─── Demo 5: Axon::execute_simple() ────────────────────────────

/// Shows `Axon::execute_simple()` for resource-free execution.
#[derive(Clone, Copy)]
struct ExecuteSimpleDemo;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ExecutionReport {
    direct_result: String,
    simple_result: String,
    note: String,
}

#[async_trait]
impl Transition<(), ExecutionReport> for ExecuteSimpleDemo {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        _input: (),
        _res: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<ExecutionReport, Self::Error> {
        // Build a sub-axon and execute it inline
        let sub_axon = Axon::simple::<String>("sub-computation").then(ComputeSquare);

        // v0.43: execute_simple() — no resources needed
        let simple_result = sub_axon.execute_simple((), bus).await;

        // Before v0.43, you had to write:
        //   let result = sub_axon.execute((), &(), bus).await;

        Outcome::Next(ExecutionReport {
            direct_result: "42^2 computed via sub-axon".into(),
            simple_result: format!("{:?}", simple_result),
            note: "execute_simple(input, bus) == execute(input, &(), bus)".into(),
        })
    }
}

#[derive(Clone, Copy)]
struct ComputeSquare;

#[async_trait]
impl Transition<(), String> for ComputeSquare {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        _input: (),
        _res: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        Outcome::Next(format!("{}", 42 * 42))
    }
}

// ─── Main ───────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config = AppConfig {
        app_name: "Outcome Patterns Demo".into(),
        version: "0.50.0".into(),
        max_retries: 3,
    };

    let try_outcome_axon = Axon::simple::<String>("try-outcome").then(TryOutcomeDemo);
    let combinator_axon = Axon::simple::<String>("combinators").then(CombinatorDemo);
    let get_cloned_axon = Axon::simple::<String>("get-cloned").then(GetClonedDemo);
    let json_outcome_axon = Axon::simple::<String>("json-outcome").then(JsonOutcomeDemo);
    let execute_simple_axon = Axon::simple::<String>("execute-simple").then(ExecuteSimpleDemo);

    println!("╔═══════════════════════════════════════════════╗");
    println!("║  Outcome Patterns Demo — Ranvier v0.42+       ║");
    println!("║  http://localhost:3200/api/*                   ║");
    println!("╚═══════════════════════════════════════════════╝");

    Ranvier::http()
        .bind("127.0.0.1:3200")
        .bus_injector({
            let config = config.clone();
            move |_parts, bus| {
                bus.insert(config.clone());
            }
        })
        .guard(AccessLogGuard::<()>::new())
        .guard(CorsGuard::<()>::permissive())
        .get_json_out("/api/try-outcome", try_outcome_axon)
        .get_json_out("/api/combinators", combinator_axon)
        .get_json_out("/api/get-cloned", get_cloned_axon)
        .get("/api/json-outcome", json_outcome_axon)
        .get_json_out("/api/execute-simple", execute_simple_axon)
        .run(())
        .await
}
