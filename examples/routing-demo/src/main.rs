/*!
# Routing Demo - Axon 기반 라우팅 패턴

## 예제 목적
Axon 패턴과 Outcome 기반 제어 흐름을 사용하여 **라우팅 결정**을 어떻게 표현하는지 보여줍니다.

## 학습 내용
- **Transition Trait**: 라우팅 로직을 상태 전이로 모델링
- **Outcome::Branch**: 경로 접두사 기반 분기
- **중첩 라우팅**: `/api/v1/users/:id/*` 구조 처리
- **타입 안전한 라우팅**: 문자열 기반 매칭을 타입 기반으로 대체

## 실행 방법
```bash
cargo run --bin routing-demo
```

## 예제 구성
1. **Path Routing**: 기본 경로 매칭 (`/`, `/status`, `/submit`)
2. **Nested Routes**: API 버전별 중첩 경로 (`/api/v1/users/:id/posts`)
3. **Branch Routing**: `Outcome::Branch`를 활용한 접두사 기반 라우팅

## 전통적 라우팅 vs Axon 라우팅
| 전통적 (예: Axum) | Axon 패턴 |
|------------------|----------|
| `Router::new().route("/users", get(users_handler))` | `Axon::start(req, "UsersRoute").then(UsersTransition)` |
| `Path::<u64>:from(param)` | 상태 타입으로 직접 전달 |
| `match` 혹은 매크로로 경로 정의 | `Transition` trait으로 라우팅 계약 |

## 참고
이 예제는 HTTP 라우팅의 구조를 보여주지만, 실제 HTTP 처리가 아닌
**결정 트리 패턴**을 어떻게 Axon으로 표현하는지에 초점을 둡니다.
*/

//! # Routing Demo - Axon-Based Routing Pattern
//!
//! This example demonstrates how routing decisions can be encoded
//! using the Axon pattern with Outcome-based control flow.
//!
//! Instead of traditional HTTP routing, this shows how the same
//! decision tree pattern applies to any routing/branching logic.

mod routes;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Ranvier Routing Demo ===\n");

    // Example 1: Simple path routing
    println!("--- Example 1: Path Routing ---");
    routes::demo_path_routing().await;

    // Example 2: Nested route resolution
    println!("\n--- Example 2: Nested Routes ---");
    routes::demo_nested_routing().await;

    // Example 3: Branch-based routing with Outcome
    println!("\n--- Example 3: Branch Outcome ---");
    routes::demo_branch_routing().await?;

    println!("\n✅ Routing demo complete!");
    Ok(())
}
