/*!
# Hello World 예제

## 예제 목적
Ranvier의 가장 기초적인 **Axon 빌더 패턴**과 **Outcome 기반 제어 흐름**을 보여줍니다.

## 학습 내용
- **Axon::start()**: 실행 경로의 시작점 정의
- **Axon::then()**: Transition을 순차적으로 연결
- **Outcome::Next**: 선형 실행 흐름 표현
- **Schematic**: 실행 경로의 자동 생성되는 구조 정보

## 실행 방법
```bash
cargo run --bin hello-world
```

## 기능 설명
이 예제는 Ranvier를 시작하기 가장 간단한 진입점입니다.
1. 빈 상태 `()` 에서 시작하여 문자열 상태로 변환
2. 두 개의 Transition(`Greet`, `Exclaim`)을 체이닝
3. 실행 후 Schematic 노드 수 확인
*/

//! # Hello World Demo - Minimal Axon Example
//!
//! This is the simplest possible example showing the Axon pattern.
//! It demonstrates linear execution flow with Outcome-based control.

use async_trait::async_trait;
use ranvier_core::prelude::*;

// ============================================================================
// 1. Define Simple Transitions
// ============================================================================

/// 첫 번째 Transition: 빈 상태에서 인사말 생성
#[derive(Clone)]
struct Greet;

#[async_trait]
impl Transition<(), String> for Greet {
    type Error = anyhow::Error;

    async fn execute(&self, _state: (), _bus: &mut Bus) -> anyhow::Result<Outcome<String, Self::Error>> {
        Ok(Outcome::Next("Hello, Ranvier!".to_string()))
    }
}

/// 두 번째 Transition: 문자열에 이모지 추가
#[derive(Clone)]
struct Exclaim;

#[async_trait]
impl Transition<String, String> for Exclaim {
    type Error = anyhow::Error;

    async fn execute(&self, state: String, _bus: &mut Bus) -> anyhow::Result<Outcome<String, Self::Error>> {
        Ok(Outcome::Next(format!("{} 🚀", state)))
    }
}

// ============================================================================
// 2. Main - Build and Execute Axon
// ============================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Hello World Demo ===\n");

    // Build a simple linear Axon
    let axon = Axon::start((), "HelloWorld")
        .then(Greet)
        .then(Exclaim);

    // Extract schematic before execution (since execute() takes ownership)
    let node_count = axon.schematic.nodes.len();

    // Execute
    let mut bus = Bus::new(http::Request::new(()));
    let result = axon.execute(&mut bus).await?;

    // Print result
    match result {
        Outcome::Next(message) => println!("{}", message),
        _ => println!("Unexpected outcome: {:?}", result),
    }

    println!("\n=== Schematic Nodes ===");
    println!("Total nodes: {}", node_count);

    Ok(())
}
