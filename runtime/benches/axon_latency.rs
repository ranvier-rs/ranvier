use criterion::{Criterion, criterion_group, criterion_main};
use ranvier_core::{Bus, Outcome, Transition};
use ranvier_runtime::Axon;
use tokio::runtime::Runtime;

#[derive(Clone)]
struct SimpleOutput;

#[derive(Debug)]
struct SimpleError;

impl std::fmt::Display for SimpleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SimpleError")
    }
}

impl std::error::Error for SimpleError {}
impl From<anyhow::Error> for SimpleError {
    fn from(_: anyhow::Error) -> Self {
        SimpleError
    }
}

#[derive(Clone)]
struct FastTransition;

#[async_trait::async_trait]
impl Transition<(), SimpleOutput> for FastTransition {
    type Error = SimpleError;
    type Resources = ();

    async fn run(
        &self,
        _state: (),
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<SimpleOutput, Self::Error> {
        // Minimum possible overhead
        Outcome::Next(SimpleOutput)
    }
}

fn bench_axon_latency(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let axon = Axon::new("fast_axon").then(FastTransition);

    // Axon is internally Arc-wrapped executor, no need to Arc it again.
    let axon = axon.clone();

    c.bench_function("axon_execute", |b| {
        b.to_async(&rt).iter(|| async {
            let mut bus = Bus::new();
            let _ = axon.execute((), &(), &mut bus).await;
        })
    });
}

criterion_group!(benches, bench_axon_latency);
criterion_main!(benches);
