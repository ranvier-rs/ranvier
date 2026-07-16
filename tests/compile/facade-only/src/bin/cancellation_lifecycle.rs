use ranvier::{
    Axon, Bus, CancellationReason, CancellationToken, ExecutionTerminal, Ranvier,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let root = CancellationToken::new();
    let workflow = root.child_token();
    let axon = Axon::<(), (), String>::new("cancellation-contract");
    let mut bus = Bus::new();

    workflow.cancel(CancellationReason::Explicit);
    match axon
        .execute_cancellable((), &(), &mut bus, workflow)
        .await
    {
        ExecutionTerminal::Cancelled(context) => {
            assert_eq!(context.reason, CancellationReason::Explicit);
        }
        ExecutionTerminal::Outcome(_) => panic!("pre-cancelled workflow must not execute"),
    }

    let ingress = Ranvier::http().bind("127.0.0.1:0");
    let _managed_server = ingress.run_with_cancellation((), root);

    Ok(())
}
