// Generated from public web strings by web/scripts/extract-getting-started-report-fixture.mjs.
#[rustfmt::skip] // Preserve the exact exported source for drift checking.
mod en {
use ranvier::prelude::*;
#[derive(Clone)]
struct BuildGreeting;
#[async_trait::async_trait]
impl Transition<String, String> for BuildGreeting {
    type Error = String;
    type Resources = ();
    async fn run(&self, input: String, _: &(), _: &mut Bus) -> Outcome<String, String> {
        Outcome::Next(format!("Hello, {input}!"))
    }
}
#[tokio::test]
async fn greeting_demo() {
let axon = Axon::<String, String, String>::new("greeting")
    .then(BuildGreeting);

let mut bus = Bus::new();
let report = axon.execute("World".into(), &(), &mut bus).await;
// This local demo intentionally has no persistence store.
match report {
    ExecutionReport::Unpersisted { result: Outcome::Next(greeting), .. } => println!("{greeting}"),
    _ => eprintln!("Execution requires review before retry"),
}
}
#[tokio::test]
async fn test_pipeline() {
    let axon = Axon::<String, String, String>::new("greeting").then(BuildGreeting);
    let mut bus = Bus::new();
    let report = axon.execute("World".into(), &(), &mut bus).await;
    assert!(matches!(report,
        ExecutionReport::Unpersisted { result: Outcome::Next(ref greeting), .. }
        if greeting == "Hello, World!"
    ));
}
}

#[rustfmt::skip] // Preserve the exact exported source for drift checking.
mod ko {
use ranvier::prelude::*;
#[derive(Clone)]
struct BuildGreeting;
#[async_trait::async_trait]
impl Transition<String, String> for BuildGreeting {
    type Error = String;
    type Resources = ();
    async fn run(&self, input: String, _: &(), _: &mut Bus) -> Outcome<String, String> {
        Outcome::Next(format!("Hello, {input}!"))
    }
}
#[tokio::test]
async fn greeting_demo() {
let axon = Axon::<String, String, String>::new("greeting")
    .then(BuildGreeting);

let mut bus = Bus::new();
let report = axon.execute("World".into(), &(), &mut bus).await;
// 이 로컬 예제는 의도적으로 persistence store를 사용하지 않습니다.
match report {
    ExecutionReport::Unpersisted { result: Outcome::Next(greeting), .. } => println!("{greeting}"),
    _ => eprintln!("재시도 전에 실행 상태를 확인하세요"),
}
}
#[tokio::test]
async fn test_pipeline() {
    let axon = Axon::<String, String, String>::new("greeting").then(BuildGreeting);
    let mut bus = Bus::new();
    let report = axon.execute("World".into(), &(), &mut bus).await;
    assert!(matches!(report,
        ExecutionReport::Unpersisted { result: Outcome::Next(ref greeting), .. }
        if greeting == "Hello, World!"
    ));
}
}
