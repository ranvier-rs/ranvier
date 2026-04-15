use criterion::{Criterion, black_box, criterion_group, criterion_main};
use ranvier_core::prelude::*;

fn bench_bus_insert_read_1(c: &mut Criterion) {
    c.bench_function("bus_insert_read_1_type", |b| {
        b.iter(|| {
            let mut bus = Bus::new();
            bus.insert(black_box(42_u64));
            let _ = bus.read::<u64>();
        });
    });
}

fn bench_bus_insert_read_10(c: &mut Criterion) {
    c.bench_function("bus_insert_read_10_types", |b| {
        b.iter(|| {
            let mut bus = Bus::new();
            bus.insert(black_box(1_u8));
            bus.insert(black_box(2_u16));
            bus.insert(black_box(3_u32));
            bus.insert(black_box(4_u64));
            bus.insert(black_box(5_i8));
            bus.insert(black_box(6_i16));
            bus.insert(black_box(7_i32));
            bus.insert(black_box(8_i64));
            bus.insert(black_box(9.0_f32));
            bus.insert(black_box(10.0_f64));
            let _ = bus.read::<u64>();
            let _ = bus.read::<f64>();
            let _ = bus.read::<i32>();
        });
    });
}

fn bench_bus_insert_read_100(c: &mut Criterion) {
    // Pre-create strings to insert as distinct types via newtype wrappers
    c.bench_function("bus_insert_string_read", |b| {
        b.iter(|| {
            let mut bus = Bus::new();
            bus.insert(black_box("hello".to_string()));
            bus.insert(black_box(vec![1_u8, 2, 3]));
            bus.insert(black_box(vec![1_u64, 2, 3]));
            bus.insert(black_box(42_u64));
            bus.insert(black_box(true));
            let _ = bus.read::<String>();
            let _ = bus.read::<Vec<u8>>();
            let _ = bus.read::<u64>();
            let _ = bus.read::<bool>();
        });
    });
}

fn bench_bus_remove(c: &mut Criterion) {
    c.bench_function("bus_insert_remove", |b| {
        b.iter(|| {
            let mut bus = Bus::new();
            bus.insert(black_box(42_u64));
            bus.insert(black_box("test".to_string()));
            let _ = bus.remove::<u64>();
            let _ = bus.remove::<String>();
        });
    });
}

criterion_group!(
    benches,
    bench_bus_insert_read_1,
    bench_bus_insert_read_10,
    bench_bus_insert_read_100,
    bench_bus_remove
);
criterion_main!(benches);
