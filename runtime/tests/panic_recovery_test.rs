use ranvier_core::prelude::*;
use ranvier_runtime::Axon;

// ── Test 1: Panic with E=String → Outcome::Fault ──────────────────

#[derive(Clone)]
struct PanickingStep;

#[async_trait::async_trait]
impl Transition<String, String> for PanickingStep {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        _state: String,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        panic!("intentional panic in transition");
    }
}

#[tokio::test]
async fn panic_in_transition_returns_fault_when_e_is_string() {
    let axon = Axon::<String, String, String>::new("PanicTest").then(PanickingStep);

    let mut bus = Bus::new();
    let result = axon.execute("hello".to_string(), &(), &mut bus).await;

    match result {
        Outcome::Fault(e) => {
            assert!(
                e.contains("panicked"),
                "Fault message should mention panic, got: {e}"
            );
        }
        other => panic!("Expected Outcome::Fault, got: {other:?}"),
    }
}

// ── Test 2: Normal transition is unaffected by catch_unwind ───────

#[derive(Clone)]
struct NormalStep;

#[async_trait::async_trait]
impl Transition<String, String> for NormalStep {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        state: String,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        Outcome::next(format!("processed: {state}"))
    }
}

#[tokio::test]
async fn normal_transition_unaffected_by_catch_unwind() {
    let axon = Axon::<String, String, String>::new("NormalTest").then(NormalStep);

    let mut bus = Bus::new();
    let result = axon.execute("hello".to_string(), &(), &mut bus).await;

    match result {
        Outcome::Next(val) => {
            assert_eq!(val, "processed: hello");
        }
        other => panic!("Expected Outcome::Next, got: {other:?}"),
    }
}

// ── Test 3: Panic with non-string E → Outcome::Emit fallback ─────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StructuredError {
    pub code: u32,
    pub message: String,
}

#[derive(Clone)]
struct PanickingStructStep;

#[async_trait::async_trait]
impl Transition<String, String> for PanickingStructStep {
    type Error = StructuredError;
    type Resources = ();

    async fn run(
        &self,
        _state: String,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        panic!("structured error panic");
    }
}

#[tokio::test]
async fn panic_with_non_string_error_emits_panic_signal() {
    let axon =
        Axon::<String, String, StructuredError>::new("StructPanicTest").then(PanickingStructStep);

    let mut bus = Bus::new();
    let result = axon.execute("hello".to_string(), &(), &mut bus).await;

    // StructuredError can't deserialize from a plain string, so we get Emit fallback
    match result {
        Outcome::Emit(event_type, payload) => {
            assert_eq!(event_type, "ranvier.transition.panic");
            let payload = payload.expect("payload should exist");
            let msg = payload["message"].as_str().expect("message field");
            assert!(
                msg.contains("structured error panic"),
                "Emit payload should contain panic message, got: {msg}"
            );
        }
        Outcome::Fault(_) => {
            // This would happen if StructuredError could somehow deserialize from a string
            // which is also acceptable behavior
        }
        other => panic!("Expected Outcome::Emit or Outcome::Fault, got: {other:?}"),
    }
}

// ── Test 4: Panic in chained pipeline (second step panics) ───────

#[derive(Clone)]
struct PanickingSecondStep;

#[async_trait::async_trait]
impl Transition<String, String> for PanickingSecondStep {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        _state: String,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        panic!("second step panic");
    }
}

#[tokio::test]
async fn panic_in_second_step_of_chain_returns_fault() {
    let axon = Axon::<String, String, String>::new("ChainPanicTest")
        .then(NormalStep)
        .then(PanickingSecondStep);

    let mut bus = Bus::new();
    let result = axon.execute("hello".to_string(), &(), &mut bus).await;

    match result {
        Outcome::Fault(e) => {
            assert!(
                e.contains("panicked"),
                "Fault message should mention panic, got: {e}"
            );
        }
        other => panic!("Expected Outcome::Fault, got: {other:?}"),
    }
}
