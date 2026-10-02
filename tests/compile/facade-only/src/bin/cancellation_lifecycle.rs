use ranvier::{
    Axon, Bus, CancellationReason, CancellationToken, ExecutionReport, ExecutionTerminal, Ranvier,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let root = CancellationToken::new();
    let workflow = root.child_token();
    let axon = Axon::<(), (), String>::new("cancellation-contract");
    let mut bus = Bus::new();

    workflow.cancel(CancellationReason::Explicit);
    match axon.execute_cancellable((), &(), &mut bus, workflow).await {
        ExecutionReport::Unpersisted {
            result: ExecutionTerminal::Cancelled(context),
            cleanup: None,
        } => {
            assert_eq!(context.reason, CancellationReason::Explicit);
        }
        _ => panic!("expected an unpersisted cancellation report without invoking the workflow"),
    }

    let ingress = Ranvier::http().bind("127.0.0.1:0");
    let _managed_server = ingress.run_with_cancellation((), root);

    Ok(())
}
