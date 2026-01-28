/*!
# State Tree Demo - Outcome 제어 흐름 예제

## 예제 목적
Ranvier의 **모든 Outcome variant** 를 실제로 체험해볼 수 있는 종합 예제입니다.

## 학습 내용
- **Outcome::Next**: 선형 진행
- **Outcome::Branch**: 명명된 분기 (예: "admin_route", "api_route")
- **Outcome::Jump**: 특정 노드로 점프 (루프 구현)
- **Outcome::Emit**: 사이드 이펙트 이벤트 (로깅, 관측)
- **Outcome::Fault**: 에러 핸들링
- **Bus.read()**: 컨텍스트에서 리소스 읽기
- **Bus.write()**: 컨텍스트에 리소스 쓰기

## 실행 방법
```bash
cargo run --bin state-tree-demo
```

## 예제 구성
1. **Linear Execution**: 검증 → 처리 (일반적인 흐름)
2. **Branch Outcome**: 접두사 기반 라우팅
3. **Emit Outcome**: 감사 로그 (실행 흐름 유지)
4. **Fault Handling**: 빈 입력 에러 처리
5. **Schematic Inspection**: 구조 정보 확인

## Outcome 분류
| Variant | 용도 | 상태 변화 |
|---------|------|----------|
| `Next(T)` | 선형 진행 | `T`로 다음 단계 |
| `Branch(Id, T)` | 분기 | `Id` 경로로 `T` 전달 |
| `Jump(NodeId, T)` | 점프/루프 | 특정 노드로 `T` 전달 |
| `Emit(Event, T)` | 사이드 이펙트 | 이벤트 발생 후 `T`로 계속 |
| `Fault(E)` | 에러 | 실행 종료, `E` 반환 |
*/

//! # State Tree Demo - Axon/Outcome Example
//!
//! This example demonstrates the core Ranvier philosophy after the Axon/Schematic pivot:
//! > Code structure IS execution structure
//! > Execution flows through Axons; Control flow is explicit via Outcome
//!
//! This example shows:
//! 1. Defining Transitions between states
//! 2. Using Outcome for explicit control flow (Next, Branch, Fault)
//! 3. Using the Bus for resource injection
//! 4. Building a decision tree with the Axon builder

use async_trait::async_trait;
use ranvier_core::prelude::*;

// ============================================================================
// 1. Define State Types
// ============================================================================

#[derive(Debug, Clone)]
struct ValidatedInput {
    value: String,
    is_premium: bool,
}

#[derive(Debug, Clone)]
struct ProcessedResult {
    output: String,
    processing_time_ms: u64,
}

/// Domain-specific error type
#[derive(Debug, Clone)]
enum AppError {
    ValidationFailed(String),
    ProcessingFailed(String),
}

// ============================================================================
// 2. Define Transitions (Atomic Steps)
// ============================================================================

/// Transition: String -> ValidatedInput
#[derive(Clone)]
struct ValidationTransition;

#[async_trait]
impl Transition<String, ValidatedInput> for ValidationTransition {
    type Error = AppError;

    async fn execute(&self, input: String, bus: &mut Bus) -> anyhow::Result<Outcome<ValidatedInput, Self::Error>> {
        // Check if input is valid (non-empty)
        if input.is_empty() {
            return Ok(Outcome::Fault(AppError::ValidationFailed(
                "Input cannot be empty".into(),
            )));
        }

        // Check for premium status from Bus
        let is_premium = bus.read::<PremiumStatus>().map(|s| s.0).unwrap_or(false);

        Ok(Outcome::Next(ValidatedInput {
            value: input.to_uppercase(),
            is_premium,
        }))
    }
}

/// Transition: ValidatedInput -> ProcessedResult
/// Demonstrates Branch outcome based on user type
#[derive(Clone)]
struct ProcessingTransition;

#[async_trait]
impl Transition<ValidatedInput, ProcessedResult> for ProcessingTransition {
    type Error = AppError;

    async fn execute(
        &self,
        input: ValidatedInput,
        _bus: &mut Bus,
    ) -> anyhow::Result<Outcome<ProcessedResult, Self::Error>> {
        // Simulate processing based on user type
        let output = if input.is_premium {
            format!("⭐ PREMIUM: {}", input.value)
        } else {
            format!("Standard: {}", input.value)
        };

        Ok(Outcome::Next(ProcessedResult {
            output,
            processing_time_ms: 42,
        }))
    }
}

/// Transition that demonstrates the Branch outcome
#[derive(Clone)]
struct RoutingTransition;

#[async_trait]
impl Transition<String, String> for RoutingTransition {
    type Error = AppError;

    async fn execute(
        &self,
        input: String,
        _bus: &mut Bus,
    ) -> anyhow::Result<Outcome<String, Self::Error>> {
        // Route based on input prefix
        if input.starts_with("admin:") {
            Ok(Outcome::Branch("admin_route".to_string(), input))
        } else if input.starts_with("api:") {
            Ok(Outcome::Branch("api_route".to_string(), input))
        } else {
            Ok(Outcome::Next(input))
        }
    }
}

/// Transition that demonstrates the Emit outcome (side-effect)
#[derive(Clone)]
struct AuditTransition;

#[async_trait]
impl Transition<String, String> for AuditTransition {
    type Error = AppError;

    async fn execute(
        &self,
        input: String,
        _bus: &mut Bus,
    ) -> anyhow::Result<Outcome<String, Self::Error>> {
        // Write audit event to bus (side-effect)
        let event = format!("AUDIT: Processing input of {} bytes", input.len());
        println!("[Audit] {}", event);

        // Emit the event but continue with the state
        Ok(Outcome::Emit(event, input))
    }
}

// ============================================================================
// 3. Resource Types for the Bus
// ============================================================================

#[derive(Debug, Clone)]
struct PremiumStatus(bool);

// ============================================================================
// 4. Main - Execute the State Tree
// ============================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Ranvier State Tree Demo ===\n");

    // Set up the Bus with resources
    let mut bus = Bus::new(http::Request::new(()));
    bus.write(PremiumStatus(true));

    // Example 1: Linear execution with validation
    println!("--- Example 1: Linear Execution ---");
    let input = "hello, ranvier!".to_string();
    println!("Input: {:?}\n", input);

    let axon1 = Axon::start(input.clone(), "ValidationFlow")
        .then(ValidationTransition)
        .then(ProcessingTransition);

    // Extract schematic data before execution (since execute() takes ownership)
    let schematic_name = axon1.schematic.name.clone();
    let node_count = axon1.schematic.nodes.len();
    let edge_count = axon1.schematic.edges.len();

    let result = axon1.execute(&mut bus).await?;
    match result {
        Outcome::Next(processed) => {
            println!("✓ Success: {}", processed.output);
            println!("  Time: {}ms\n", processed.processing_time_ms);
        }
        Outcome::Fault(e) => println!("✗ Error: {:?}\n", e),
        _ => println!("Unexpected outcome: {:?}\n", result),
    }

    // Example 2: Demonstrating Branch outcome
    println!("--- Example 2: Branch Outcome ---");
    let admin_input = "admin:user_management".to_string();

    let axon2 = Axon::start(admin_input, "RoutingFlow").then(RoutingTransition);
    let result2 = axon2.execute(&mut bus).await?;

    match &result2 {
        Outcome::Branch(route, data) => {
            println!("Routed to: {}", route);
            println!("Data: {}", data);
        }
        Outcome::Next(data) => println!("Default route: {}", data),
        _ => {}
    }

    // Example 3: Demonstrating Emit outcome
    println!("\n--- Example 3: Emit (Side-Effect) ---");
    let data = "sensitive_operation".to_string();

    let axon3 = Axon::start(data, "AuditFlow").then(AuditTransition);
    let result3 = axon3.execute(&mut bus).await?;

    match result3 {
        Outcome::Emit(event, state) => {
            println!("Emitted: {}", event);
            println!("Continued with: {}", state);
        }
        _ => {}
    }

    // Example 4: Fault handling
    println!("\n--- Example 4: Fault Handling ---");
    let empty_input = "".to_string();

    let axon4 = Axon::start(empty_input, "ErrorFlow").then(ValidationTransition);
    let result4 = axon4.execute(&mut bus).await?;

    match result4 {
        Outcome::Fault(e) => println!("Caught error: {:?}", e),
        Outcome::Next(_) => println!("Unexpected success"),
        _ => {}
    }

    // Example 5: Schematic inspection
    println!("\n--- Example 5: Schematic Inspection ---");
    println!("Schematic: {}", schematic_name);
    println!("Nodes: {}", node_count);
    println!("Edges: {}", edge_count);

    println!("\n✅ Demo complete - Axon/Outcome pattern demonstrated!");
    Ok(())
}
