use criterion::{Criterion, black_box, criterion_group, criterion_main};
use ranvier_core::Never;
use ranvier_core::prelude::*;
use ranvier_macros::transition;
use ranvier_runtime::prelude::*;

#[transition]
async fn identity_logic(input: String, _res: &(), _bus: &mut Bus) -> Outcome<String, Never> {
    Outcome::Next(input)
}

fn bench_axon_latency(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let axon = Axon::<String, String, Never>::new("bench").then(identity_logic);

    c.bench_function("axon_identity_latency", |b| {
        b.to_async(&rt).iter(|| async {
            let mut bus = Bus::new();
            let _ = axon
                .execute(black_box("hello".to_string()), &(), &mut bus)
                .await;
        });
    });
}

criterion_group!(benches, bench_axon_latency);
criterion_main!(benches);
