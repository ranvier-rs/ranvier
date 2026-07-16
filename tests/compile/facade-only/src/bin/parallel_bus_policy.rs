use ranvier::prelude::*;
use std::sync::Arc;

fn main() {
    let mut bus = Bus::new();
    bus.insert_shared(TenantId::new("tenant-a"));
    let branch_bus = bus.fork_for_parallel();
    assert_eq!(
        branch_bus.read::<TenantId>().map(TenantId::as_str),
        Some("tenant-a")
    );

    let transitions: Vec<
        Arc<dyn Transition<(), (), Resources = (), Error = String> + Send + Sync>,
    > = Vec::new();
    let _axon = Axon::<(), (), String, ()>::new("ParallelContext").parallel_with_bus_policy(
        transitions,
        ParallelStrategy::AllMustSucceed,
        ParallelBusPolicy::InheritShared,
    );
}
