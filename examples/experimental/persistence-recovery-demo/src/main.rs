use async_trait::async_trait;
use ranvier_core::{Bus, Outcome, Transition};
use ranvier_runtime::{
    Axon, CompensationContext, CompensationHandle, CompensationHook, InMemoryPersistenceStore,
    PersistenceAutoComplete, PersistenceHandle, PersistenceStore, PersistenceTraceId,
};
use std::sync::Arc;

#[derive(Clone, Debug)]
struct OrderFlowState {
    order_id: String,
    validated: bool,
    should_fail_payment: bool,
}

#[derive(Clone)]
struct ValidateOrder;

#[async_trait]
impl Transition<OrderFlowState, OrderFlowState> for ValidateOrder {
    type Error = &'static str;
    type Resources = ();

    async fn run(
        &self,
        mut state: OrderFlowState,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<OrderFlowState, Self::Error> {
        state.validated = true;
        Outcome::Next(state)
    }
}

#[derive(Clone)]
struct ChargePayment;

#[async_trait]
impl Transition<OrderFlowState, OrderFlowState> for ChargePayment {
    type Error = &'static str;
    type Resources = ();

    async fn run(
        &self,
        state: OrderFlowState,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<OrderFlowState, Self::Error> {
        if state.should_fail_payment {
            return Outcome::Fault("payment_declined");
        }
        Outcome::Next(state)
    }
}

#[derive(Clone)]
struct FinalizeOrder;

#[async_trait]
impl Transition<OrderFlowState, String> for FinalizeOrder {
    type Error = &'static str;
    type Resources = ();

    async fn run(
        &self,
        state: OrderFlowState,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        let tracking = format!("tracking-{}", state.order_id);
        Outcome::Next(tracking)
    }
}

fn build_order_axon() -> Axon<OrderFlowState, String, &'static str> {
    Axon::<OrderFlowState, OrderFlowState, &'static str>::start("OrderRecoveryFlow")
        .then(ValidateOrder)
        .then(ChargePayment)
        .then(FinalizeOrder)
}

fn print_trace_summary(label: &str, trace: &ranvier_runtime::PersistedTrace) {
    println!("\n== {} ==", label);
    println!("trace_id: {}", trace.trace_id);
    println!("circuit: {}", trace.circuit);
    println!("events: {}", trace.events.len());
    for event in &trace.events {
        println!(
            "  step={} outcome={} ts={}",
            event.step, event.outcome_kind, event.timestamp_ms
        );
    }
    println!("resumed_from_step: {:?}", trace.resumed_from_step);
    println!("completion: {:?}", trace.completion);
}

#[derive(Clone)]
struct RefundPaymentCompensation;

#[async_trait]
impl CompensationHook for RefundPaymentCompensation {
    async fn compensate(&self, context: CompensationContext) -> anyhow::Result<()> {
        println!(
            "[compensate] trace={} circuit={} reason={} step={}",
            context.trace_id, context.circuit, context.fault_kind, context.fault_step
        );
        // In a real service this would call an idempotent refund/reversal API.
        Ok(())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let store_impl = Arc::new(InMemoryPersistenceStore::new());
    let store_dyn: Arc<dyn PersistenceStore> = store_impl.clone();
    let handle = PersistenceHandle::from_arc(store_dyn);
    let trace_id = "order-1001";

    // First run (simulated process before crash): payment fault, keep trace open.
    let mut bus1 = Bus::new();
    bus1.insert(handle.clone());
    bus1.insert(PersistenceTraceId::new(trace_id));
    bus1.insert(PersistenceAutoComplete(false));

    let axon = build_order_axon();
    let first_input = OrderFlowState {
        order_id: "1001".to_string(),
        validated: false,
        should_fail_payment: true,
    };
    let first_outcome = axon.execute(first_input, &(), &mut bus1).await;
    println!("first run outcome: {:?}", first_outcome);

    let first_trace = store_impl
        .load(trace_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("missing trace after first run"))?;
    print_trace_summary("after first run", &first_trace);

    let resume_from = first_trace.events.last().map(|event| event.step).unwrap_or(0);
    let cursor = store_impl.resume(trace_id, resume_from).await?;
    println!(
        "\nresume cursor: trace_id={} next_step={}",
        cursor.trace_id, cursor.next_step
    );

    // Second run (simulated process after restart): fix condition and complete.
    let mut bus2 = Bus::new();
    bus2.insert(handle.clone());
    bus2.insert(PersistenceTraceId::new(trace_id));
    bus2.insert(PersistenceAutoComplete(true));

    let second_input = OrderFlowState {
        order_id: "1001".to_string(),
        validated: false,
        should_fail_payment: false,
    };
    let second_outcome = axon.execute(second_input, &(), &mut bus2).await;
    println!("second run outcome: {:?}", second_outcome);

    let final_trace = store_impl
        .load(trace_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("missing trace after second run"))?;
    print_trace_summary("after second run", &final_trace);

    // Third run (compensation demo): fault with irreversible side-effect compensation hook.
    let compensation_trace_id = "order-2001";
    let mut bus3 = Bus::new();
    bus3.insert(handle);
    bus3.insert(PersistenceTraceId::new(compensation_trace_id));
    bus3.insert(CompensationHandle::from_hook(RefundPaymentCompensation));

    let compensation_input = OrderFlowState {
        order_id: "2001".to_string(),
        validated: false,
        should_fail_payment: true,
    };
    let compensation_outcome = axon.execute(compensation_input, &(), &mut bus3).await;
    println!("third run (compensation) outcome: {:?}", compensation_outcome);

    let compensation_trace = store_impl
        .load(compensation_trace_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("missing trace after compensation run"))?;
    print_trace_summary("after compensation run", &compensation_trace);

    Ok(())
}
