use criterion::{black_box, criterion_group, criterion_main, Criterion};
use ranvier_core::prelude::*;
use ranvier_runtime::prelude::*;
use ranvier_macros::transition;
use ranvier_core::Never;

#[transition]
async fn increment(input: i64, _res: &(), _bus: &mut Bus) -> Outcome<i64, Never> {
    Outcome::Next(input + 1)
}

fn bench_1_step_chain(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let axon = Axon::<i64, i64, Never>::new("chain-1")
        .then(increment);

    c.bench_function("transition_chain_1_step", |b| {
        b.to_async(&rt).iter(|| async {
            let mut bus = Bus::new();
            let _ = axon.execute(black_box(0), &(), &mut bus).await;
        });
    });
}

fn bench_3_step_chain(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let axon = Axon::<i64, i64, Never>::new("chain-3")
        .then(increment)
        .then(increment)
        .then(increment);

    c.bench_function("transition_chain_3_step", |b| {
        b.to_async(&rt).iter(|| async {
            let mut bus = Bus::new();
            let _ = axon.execute(black_box(0), &(), &mut bus).await;
        });
    });
}

fn bench_10_step_chain(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let axon = Axon::<i64, i64, Never>::new("chain-10")
        .then(increment)
        .then(increment)
        .then(increment)
        .then(increment)
        .then(increment)
        .then(increment)
        .then(increment)
        .then(increment)
        .then(increment)
        .then(increment);

    c.bench_function("transition_chain_10_step", |b| {
        b.to_async(&rt).iter(|| async {
            let mut bus = Bus::new();
            let _ = axon.execute(black_box(0), &(), &mut bus).await;
        });
    });
}

criterion_group!(benches, bench_1_step_chain, bench_3_step_chain, bench_10_step_chain);
criterion_main!(benches);
