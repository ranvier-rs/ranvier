/*!
# Basic Schematic 예제

## 예제 목적
Ranvier의 핵심 철학인 **"코드 구조 = 실행 구조"** 를 보여줍니다.
Axon으로 실행을 정의하면 동시에 Schematic(구조 분석 정보)이 자동 생성됩니다.

## 학습 내용
- **Transition Trait**: 상태 간 변환을 정의하는 계약
- **Axon Builder**: `start()` → `then()` 체이닝으로 실행 경로 구성
- **Schematic 추출**: 실행 경로의 JSON 직렬화
- **Bus**: 리소스 주입을 위한 컨텍스트 객체

## 실행 방법
```bash
cargo run --bin basic-schematic
```

## 출력 예시
```
=== Schematic Definition (JSON) ===
{
  "name": "My First Schematic",
  "description": null,
  "nodes": [...],
  "edges": [...]
}
===================================
```

## Schematic vs Axon
| 개념 | 역할 | 데이터 |
|------|------|--------|
| **Axon** | 실행 경로 (런타임) | `Outcome<T, E>` |
| **Schematic** | 구조 분석 (정적) | `Schematic` JSON |
*/

//! # Basic Schematic Demo - Axon/Schematic Example
//!
//! This example demonstrates the core Ranvier philosophy after the Axon/Schematic pivot:
//! > Code structure IS execution structure
//! > Execution flows through Axons; Structure is captured in Schematics
//!
//! This is a minimal example showing:
//! 1. Using the Axon builder to chain execution
//! 2. Implementing Transitions between states
//! 3. Using the Bus for resource injection
//! 4. Extracting Schematic metadata (the structure)

use async_trait::async_trait;
use ranvier_core::prelude::*;

// ============================================================================
// 1. Define Transitions (Atomic Steps)
// ============================================================================

/// Transition: () -> String (로그 시작)
#[derive(Clone)]
struct LogStart;

#[async_trait]
impl Transition<(), String> for LogStart {
    type Error = anyhow::Error;

    async fn run(&self, _state: (), _bus: &mut Bus) -> Outcome<String, Self::Error> {
        println!("[Axon] Circuit started.");
        Outcome::Next("Initial state".to_string())
    }
}

/// Transition: String -> String (데이터 처리)
#[derive(Clone)]
struct ProcessData;

#[async_trait]
impl Transition<String, String> for ProcessData {
    type Error = anyhow::Error;

    async fn run(&self, state: String, _bus: &mut Bus) -> Outcome<String, Self::Error> {
        println!("[Axon] Processing data: {}", state);
        Outcome::Next(format!("Processed: {}", state))
    }
}

/// Transition: String -> () (로그 종료)
#[derive(Clone)]
struct LogEnd;

#[async_trait]
impl Transition<String, ()> for LogEnd {
    type Error = anyhow::Error;

    async fn run(&self, state: String, _bus: &mut Bus) -> Outcome<(), Self::Error> {
        println!("[Axon] Circuit ended with: {}", state);
        Outcome::Next(())
    }
}

// ============================================================================
// 2. Main - Build Axon and Execute
// ============================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Build the Axon (Execution chain)
    let axon = Axon::start((), "My First Schematic")
        .then(LogStart)
        .then(ProcessData)
        .then(LogEnd);

    // Extract Schematic (Static structure) before execution
    let schematic_json = serde_json::to_string_pretty(&axon.schematic)?;

    // If running from CLI for schematic extraction, print JSON and exit
    if std::env::var("RANVIER_SCHEMATIC").is_ok() {
        println!("{}", schematic_json);
        return Ok(());
    }

    let node_count = axon.schematic.nodes.len();
    let edge_count = axon.schematic.edges.len();

    println!("=== Schematic Definition (JSON) ===");
    println!("{}", schematic_json);
    println!("===================================\n");

    // Execute the Axon
    println!("=== Running Axon ===");
    let mut bus = Bus::new();
    let result = axon.execute(&mut bus).await;
    println!("Final Result: {:?}", result);

    // Demonstrate Axon helper methods
    println!("\n=== Axon Analysis ===");
    println!("Total nodes: {}", node_count);
    println!("Total edges: {}", edge_count);

    Ok(())
}
