//! M173 Completion Criteria: Complex Order-Saga Demo
//!
//! Demonstrates a realistic e-commerce order workflow with:
//! - Multi-step saga (reserve inventory → charge payment → confirm shipment)
//! - Simulated failure at the payment step
//! - Automated LIFO compensation (undo reservation, refund placeholder)
//! - Dead-letter queue capture of the failed event
//! - Persistence trace of the full execution history
//! - Hot-reloadable DLQ policy via DynamicPolicy

use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_core::saga::SagaPolicy;
use ranvier_runtime::Axon;
use ranvier_runtime::persistence::{
    CompensationAutoTrigger, CompensationContext, CompensationHandle, CompensationHook,
    InMemoryPersistenceStore, PersistenceHandle, PersistenceStore, PersistenceTraceId,
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

// ── Domain Types ────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct OrderState {
    order_id: String,
    amount: f64,
    inventory_reserved: bool,
    payment_charged: bool,
    shipment_confirmed: bool,
    audit_log: Vec<String>,
}

impl OrderState {
    fn new(order_id: &str, amount: f64) -> Self {
        Self {
            order_id: order_id.to_string(),
            amount,
            inventory_reserved: false,
            payment_charged: false,
            shipment_confirmed: false,
            audit_log: vec![],
        }
    }
}

// ── Transitions ─────────────────────────────────────────────

#[derive(Clone)]
struct ReserveInventory;

#[async_trait]
impl Transition<OrderState, OrderState> for ReserveInventory {
    type Error = String;
    type Resources = ();
    fn label(&self) -> String {
        "ReserveInventory".to_string()
    }

    async fn run(
        &self,
        mut state: OrderState,
        _res: &(),
        _bus: &mut Bus,
    ) -> Outcome<OrderState, String> {
        state.inventory_reserved = true;
        state
            .audit_log
            .push(format!("Inventory reserved for order {}", state.order_id));
        Outcome::Next(state)
    }
}

#[derive(Clone)]
struct UndoReserveInventory {
    log: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Transition<OrderState, ()> for UndoReserveInventory {
    type Error = String;
    type Resources = ();
    fn label(&self) -> String {
        "UndoReserveInventory".to_string()
    }

    async fn run(&self, state: OrderState, _res: &(), _bus: &mut Bus) -> Outcome<(), String> {
        let msg = format!(
            "Compensation: Released inventory for order {} (was_reserved={})",
            state.order_id, state.inventory_reserved
        );
        self.log.lock().unwrap().push(msg);
        Outcome::Next(())
    }
}

#[derive(Clone)]
struct ChargePayment {
    should_fail: bool,
}

#[async_trait]
impl Transition<OrderState, OrderState> for ChargePayment {
    type Error = String;
    type Resources = ();
    fn label(&self) -> String {
        "ChargePayment".to_string()
    }

    async fn run(
        &self,
        mut state: OrderState,
        _res: &(),
        _bus: &mut Bus,
    ) -> Outcome<OrderState, String> {
        if self.should_fail {
            state.audit_log.push(format!(
                "Payment FAILED for order {} (amount={})",
                state.order_id, state.amount
            ));
            return Outcome::fault(format!(
                "Payment gateway declined order {} for ${:.2}",
                state.order_id, state.amount
            ));
        }
        state.payment_charged = true;
        state.audit_log.push(format!(
            "Payment charged ${:.2} for order {}",
            state.amount, state.order_id
        ));
        Outcome::Next(state)
    }
}

#[derive(Clone)]
struct UndoChargePayment {
    log: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Transition<OrderState, ()> for UndoChargePayment {
    type Error = String;
    type Resources = ();
    fn label(&self) -> String {
        "UndoChargePayment".to_string()
    }

    async fn run(&self, state: OrderState, _res: &(), _bus: &mut Bus) -> Outcome<(), String> {
        let msg = format!(
            "Compensation: Refund placeholder for order {} (was_charged={})",
            state.order_id, state.payment_charged
        );
        self.log.lock().unwrap().push(msg);
        Outcome::Next(())
    }
}

#[derive(Clone)]
struct ConfirmShipment;

#[async_trait]
impl Transition<OrderState, OrderState> for ConfirmShipment {
    type Error = String;
    type Resources = ();
    fn label(&self) -> String {
        "ConfirmShipment".to_string()
    }

    async fn run(
        &self,
        mut state: OrderState,
        _res: &(),
        _bus: &mut Bus,
    ) -> Outcome<OrderState, String> {
        state.shipment_confirmed = true;
        state
            .audit_log
            .push(format!("Shipment confirmed for order {}", state.order_id));
        Outcome::Next(state)
    }
}

// ── Mock DLQ Sink ───────────────────────────────────────────

#[derive(Clone)]
struct TestDlqSink {
    letters: Arc<tokio::sync::Mutex<Vec<String>>>,
}

#[async_trait]
impl DlqSink for TestDlqSink {
    async fn store_dead_letter(
        &self,
        workflow_id: &str,
        circuit_label: &str,
        node_id: &str,
        error_msg: &str,
        _payload: &[u8],
    ) -> Result<(), String> {
        self.letters.lock().await.push(format!(
            "DLQ[{}:{}:{}]: {}",
            workflow_id, circuit_label, node_id, error_msg
        ));
        Ok(())
    }
}

// ── Mock Compensation Hook ──────────────────────────────────

struct OrderCompensationHook {
    triggered: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl CompensationHook for OrderCompensationHook {
    async fn compensate(&self, ctx: CompensationContext) -> anyhow::Result<()> {
        self.triggered.lock().unwrap().push(format!(
            "Hook: {} fault_kind={} at step {}",
            ctx.trace_id, ctx.fault_kind, ctx.fault_step
        ));
        Ok(())
    }
}

// ── Tests ───────────────────────────────────────────────────

#[tokio::test]
async fn order_saga_encounters_failure_and_compensates_perfectly() {
    // Setup infrastructure
    let store = Arc::new(InMemoryPersistenceStore::new());
    let compensation_log = Arc::new(Mutex::new(Vec::<String>::new()));
    let hook_log = Arc::new(Mutex::new(Vec::<String>::new()));
    let dlq_sink = TestDlqSink {
        letters: Arc::new(tokio::sync::Mutex::new(Vec::new())),
    };

    // Build the order saga: ReserveInventory → ChargePayment(FAIL) → ConfirmShipment
    // Compensated steps prioritize saga compensation over retry.
    // DLQ captures the failure regardless.
    let axon = Axon::<OrderState, OrderState, String, ()>::new("OrderSaga")
        .with_saga_policy(SagaPolicy::Enabled)
        .with_dlq_policy(DlqPolicy::SendToDlq)
        .with_dlq_sink(dlq_sink.clone())
        .then_compensated(
            ReserveInventory,
            UndoReserveInventory {
                log: compensation_log.clone(),
            },
        )
        .then_compensated(
            ChargePayment { should_fail: true },
            UndoChargePayment {
                log: compensation_log.clone(),
            },
        )
        .then(ConfirmShipment);

    // Execute with persistence and compensation enabled
    let mut bus = Bus::new();
    bus.insert(PersistenceHandle::from_arc(
        store.clone() as Arc<dyn PersistenceStore>
    ));
    bus.insert(PersistenceTraceId::new("order-001"));
    bus.insert(CompensationHandle::from_hook(OrderCompensationHook {
        triggered: hook_log.clone(),
    }));
    bus.insert(CompensationAutoTrigger(true));
    bus.insert(Timeline::new());

    let order = OrderState::new("ORD-2027-001", 149.99);
    let outcome = axon.execute(order, &(), &mut bus).await;

    // ━━━ Verification ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

    // 1. Outcome should be a Fault (payment declined)
    match &outcome {
        Outcome::Fault(msg) => {
            assert!(
                msg.contains("Payment gateway declined"),
                "Fault message should mention payment: {}",
                msg
            );
        }
        other => panic!("Expected Fault, got {:?}", other),
    }

    // 2. Saga compensation occurred in LIFO order
    let comp_log = compensation_log.lock().unwrap();
    assert_eq!(
        comp_log.len(),
        2,
        "Should have 2 compensation entries (ChargePayment + ReserveInventory)"
    );
    // LIFO: ChargePayment compensation first, then ReserveInventory
    assert!(
        comp_log[0].contains("Refund placeholder"),
        "First compensation should be payment undo: {}",
        comp_log[0]
    );
    assert!(
        comp_log[0].contains("was_charged=false"),
        "Payment was never charged (state captured BEFORE the step): {}",
        comp_log[0]
    );
    assert!(
        comp_log[1].contains("Released inventory"),
        "Second compensation should be inventory undo: {}",
        comp_log[1]
    );
    assert!(
        comp_log[1].contains("was_reserved=false"),
        "Inventory state captured BEFORE step 1: {}",
        comp_log[1]
    );

    // 3. Compensation hook was triggered
    let hooks = hook_log.lock().unwrap();
    assert_eq!(hooks.len(), 1, "Compensation hook should fire once");
    assert!(
        hooks[0].contains("order-001"),
        "Hook should reference the trace_id"
    );

    // 4. DLQ captured the failed event (after retry exhaustion)
    let letters = dlq_sink.letters.lock().await;
    assert_eq!(letters.len(), 1, "Should have 1 dead letter");
    assert!(
        letters[0].contains("OrderSaga"),
        "Dead letter should reference the circuit: {}",
        letters[0]
    );

    // 5. Persistence trace contains the full history
    let trace = store.load("order-001").await.unwrap().unwrap();
    assert!(
        trace.events.len() >= 3,
        "Should have at least 3 persistence events"
    );
    assert_eq!(trace.events[0].outcome_kind, "Enter");

    // 6. Timeline has execution trace
    let timeline = bus.read::<Timeline>().unwrap();
    assert!(
        !timeline.events.is_empty(),
        "Timeline should have recorded execution events"
    );
    // Verify at least NodeEnter/NodeExit events were tracked
    let enter_count = timeline
        .events
        .iter()
        .filter(|e| matches!(e, TimelineEvent::NodeEnter { .. }))
        .count();
    assert!(
        enter_count >= 2,
        "Should have at least 2 NodeEnter events (Reserve + Charge)"
    );
}

#[tokio::test]
async fn order_saga_succeeds_end_to_end_without_failure() {
    let compensation_log = Arc::new(Mutex::new(Vec::<String>::new()));

    let axon = Axon::<OrderState, OrderState, String, ()>::new("OrderSagaSuccess")
        .with_saga_policy(SagaPolicy::Enabled)
        .then_compensated(
            ReserveInventory,
            UndoReserveInventory {
                log: compensation_log.clone(),
            },
        )
        .then_compensated(
            ChargePayment { should_fail: false },
            UndoChargePayment {
                log: compensation_log.clone(),
            },
        )
        .then(ConfirmShipment);

    let mut bus = Bus::new();
    let order = OrderState::new("ORD-SUCCESS", 99.99);
    let outcome = axon.execute(order, &(), &mut bus).await;

    // Should succeed through all 3 steps
    match &outcome {
        Outcome::Next(state) => {
            assert!(state.inventory_reserved, "Inventory should be reserved");
            assert!(state.payment_charged, "Payment should be charged");
            assert!(state.shipment_confirmed, "Shipment should be confirmed");
            assert_eq!(state.audit_log.len(), 3, "Should have 3 audit entries");
        }
        other => panic!("Expected Next, got {:?}", other),
    }

    // No compensation should have fired
    assert!(
        compensation_log.lock().unwrap().is_empty(),
        "No compensation needed for successful order"
    );
}
