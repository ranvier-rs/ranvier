use super::*;
fn capacity_provider(mode: crate::redaction::Mode) -> ProtectedMeterProvider {
    ProtectedMeterProvider {
        inner: SdkMeterProvider::builder().build(),
        policy: Policy::new(mode, "").unwrap(),
        state: Default::default(),
    }
}
#[test]
fn concurrent_meter_registration_stays_within_provider_budget() {
    let state = MetricState::default();
    std::thread::scope(|threads| {
        for worker in 0..8 {
            let state = state.clone();
            threads.spawn(move || {
                for index in 0..32 {
                    state.register_meter(
                        &InstrumentationScope::builder(format!("scope-{worker}-{index}")).build(),
                    );
                }
            });
        }
    });
    assert!(state.failed());
    assert_eq!(state.2.lock().unwrap().len(), MAX_METERS);
    assert!(!state.register_meter(&InstrumentationScope::builder("after-failure").build()));
}

#[test]
fn provider_meter_and_instrument_capacity_accepts_reuse_and_rejects_growth() {
    let provider = capacity_provider(crate::redaction::Mode::Public);
    for index in 0..MAX_METERS {
        provider.meter_with_scope(InstrumentationScope::builder(format!("scope-{index}")).build());
    }
    provider.meter("scope-0");
    assert!(!provider.state.failed());
    assert_eq!(provider.state.2.lock().unwrap().len(), MAX_METERS);
    provider.meter("one-more");
    assert!(provider.state.failed());
    assert_eq!(provider.state.2.lock().unwrap().len(), MAX_METERS);
    let provider = capacity_provider(crate::redaction::Mode::Public);
    let meter = provider.meter("capacity");
    for index in 0..MAX_INSTRUMENTS {
        meter.u64_counter(format!("metric_{index}")).build();
    }
    meter
        .u64_counter("metric_0")
        .with_description("filtered description")
        .build();
    assert!(!provider.state.failed());
    assert_eq!(provider.state.3.lock().unwrap().len(), MAX_INSTRUMENTS);
    meter.u64_counter("one_more").build();
    assert!(provider.state.failed());
    assert_eq!(provider.state.3.lock().unwrap().len(), MAX_INSTRUMENTS);
}
#[test]
fn metadata_variants_are_bounded_before_redaction() {
    let provider = capacity_provider(crate::redaction::Mode::Off);
    let meter = provider.meter("capacity");
    meter.u64_counter("shared").build();
    meter.f64_counter("shared").build();
    meter.u64_counter("shared").with_unit("s").build();
    meter
        .u64_counter("shared")
        .with_description("description")
        .build();
    meter
        .u64_histogram("shared")
        .with_boundaries(vec![1.0])
        .build();
    meter
        .u64_histogram("shared")
        .with_boundaries(vec![2.0])
        .build();
    assert_eq!(provider.state.3.lock().unwrap().len(), 6);
    for field in ["description", "unit", "boundaries"] {
        let provider = capacity_provider(crate::redaction::Mode::Strict);
        let meter = provider.meter("capacity");
        match field {
            "description" => {
                meter
                    .u64_counter("metric")
                    .with_description("x".repeat(1025))
                    .build();
            }
            "unit" => {
                meter
                    .u64_counter("metric")
                    .with_unit("x".repeat(65))
                    .build();
            }
            _ => {
                meter
                    .u64_histogram("metric")
                    .with_boundaries(
                        (0..=MAX_HISTOGRAM_BOUNDARIES)
                            .map(|value| value as f64)
                            .collect(),
                    )
                    .build();
            }
        }
        assert!(provider.state.failed());
        assert!(provider.state.3.lock().unwrap().is_empty());
    }
}
#[test]
fn oversized_sync_and_gauge_labels_never_reach_inner_instruments() {
    struct CountObserver(std::sync::atomic::AtomicUsize);
    impl AsyncInstrument<u64> for CountObserver {
        fn observe(&self, _: u64, _: &[KeyValue]) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        }
    }
    let policy = Policy::new(crate::redaction::Mode::Strict, "").unwrap();
    let invalid = [KeyValue::new(
        "email",
        "x".repeat(MAX_OBSERVABLE_ATTRIBUTE_BYTES),
    )];
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let record_calls = calls.clone();
    let state = MetricState::default();
    let recording = Recording {
        record: Box::new(move |_: u64, _: &[KeyValue]| {
            record_calls.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        }),
        policy: policy.clone(),
        state: state.clone(),
    };
    recording.measure(1, &invalid);
    recording.measure(1, &[]);
    assert!(state.failed());
    assert_eq!(calls.load(std::sync::atomic::Ordering::Acquire), 0);
    let state = MetricState::default();
    let inner = CountObserver(std::sync::atomic::AtomicUsize::new(0));
    let observing = Observing {
        inner: &inner,
        policy: &policy,
        state: &state,
    };
    observing.observe(1, &invalid);
    observing.observe(1, &[]);
    assert!(state.failed());
    assert_eq!(inner.0.load(std::sync::atomic::Ordering::Acquire), 0);
}
#[test]
fn invalid_scope_is_rejected_before_observable_registration() {
    let scopes = [
        InstrumentationScope::builder("x".repeat(256)).build(),
        InstrumentationScope::builder("valid")
            .with_version("bad\nversion")
            .build(),
        InstrumentationScope::builder("valid")
            .with_schema_url("x".repeat(2049))
            .build(),
        InstrumentationScope::builder("valid")
            .with_attributes([KeyValue::new("email", "x".repeat(8192))])
            .build(),
        InstrumentationScope::builder("valid")
            .with_attributes(vec![KeyValue::new("x", 1); 65])
            .build(),
    ];
    for scope in scopes {
        let provider = ProtectedMeterProvider {
            inner: SdkMeterProvider::builder().build(),
            policy: Policy::new(crate::redaction::Mode::Strict, "").unwrap(),
            state: Default::default(),
        };
        let _counter = provider
            .meter_with_scope(scope)
            .u64_observable_counter("counter")
            .with_callback(|_| panic!("rejected scope callback ran"))
            .build();
        assert!(provider.state.failed());
        assert!(provider.state.1.lock().unwrap().is_empty());
        provider.inner.shutdown().unwrap();
    }
}
#[test]
fn metric_name_validation_rejects_unsafe_syntax_and_size() {
    for name in [
        "",
        "1count",
        "name secret",
        "name\n",
        "name=secret",
        "이름",
        "name:secret",
    ] {
        assert!(!valid_instrument_name(name));
    }
    assert!(valid_instrument_name(&"a".repeat(255)));
    assert!(!valid_instrument_name(&"a".repeat(256)));
    assert!(valid_instrument_name("count.total/runs-1_ok"));
    let provider = ProtectedMeterProvider {
        inner: SdkMeterProvider::builder().build(),
        policy: Policy::new(crate::redaction::Mode::Public, "").unwrap(),
        state: Default::default(),
    };
    let meter = provider.meter("fixture");
    meter.u64_counter("bad name").build().add(1, &[]);
    meter.f64_histogram("bad name").build().record(1.0, &[]);
    let _counter = meter
        .u64_observable_counter("bad name")
        .with_callback(|_| panic!("rejected callback ran"))
        .build();
    let _gauge = meter
        .u64_observable_gauge("bad name")
        .with_callback(|_| panic!("rejected callback ran"))
        .build();
    assert!(provider.state.failed());
    assert!(provider.state.1.lock().unwrap().is_empty());
    provider.inner.shutdown().unwrap();
}
#[test]
fn strict_gauge_scope_collision_fails_before_either_callback_runs() {
    let captured = Arc::new(Mutex::new(Values::new()));
    let provider = ProtectedMeterProvider {
        inner: SdkMeterProvider::builder()
            .with_reader(
                PeriodicReader::builder(CollisionCapture(captured.clone()))
                    .with_interval(Duration::from_secs(3600))
                    .build(),
            )
            .build(),
        policy: Policy::new(crate::redaction::Mode::Strict, "").unwrap(),
        state: Default::default(),
    };
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let first_calls = calls.clone();
    let _first = provider
        .meter("first-scope")
        .i64_observable_gauge("same")
        .with_callback(move |instrument| {
            first_calls.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            instrument.observe(2, &[]);
        })
        .build();
    let second_calls = calls.clone();
    let _second = provider
        .meter("second-scope")
        .i64_observable_gauge("same")
        .with_callback(move |instrument| {
            second_calls.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            instrument.observe(8, &[]);
        })
        .build();
    assert!(provider.state.failed());
    provider.inner.force_flush().unwrap();
    provider.inner.shutdown().unwrap();
    assert_eq!(calls.load(std::sync::atomic::Ordering::Acquire), 0);
    assert!(!captured.lock().unwrap().contains_key("same"));
}
#[test]
fn strict_scope_collision_is_reported_instead_of_overwriting_observable_sum() {
    let captured = Arc::new(Mutex::new(Values::new()));
    let provider = ProtectedMeterProvider {
        inner: SdkMeterProvider::builder()
            .with_reader(
                PeriodicReader::builder(CollisionCapture(captured.clone()))
                    .with_interval(Duration::from_secs(3600))
                    .build(),
            )
            .build(),
        policy: Policy::new(crate::redaction::Mode::Strict, "").unwrap(),
        state: Default::default(),
    };
    let first = provider.meter("first-distinct-scope");
    let second = provider.meter("second-distinct-scope");
    first.i64_up_down_counter("same_sync").build().add(2, &[]);
    second.i64_up_down_counter("same_sync").build().add(8, &[]);
    let _first = first
        .i64_observable_up_down_counter("same_observable")
        .with_callback(|instrument| instrument.observe(2, &[]))
        .build();
    let _second = second
        .i64_observable_up_down_counter("same_observable")
        .with_callback(|instrument| instrument.observe(8, &[]))
        .build();
    provider.inner.force_flush().unwrap();
    let values = captured.lock().unwrap().clone();
    assert_eq!(values.get("same_sync"), Some(&vec![10]));
    assert!(!values.contains_key("same_observable"));
    assert!(provider.state.failed());
    provider.inner.shutdown().unwrap();
}
#[test]
fn metric_scope_payload_is_filtered_before_export() {
    for mode in [
        crate::redaction::Mode::Public,
        crate::redaction::Mode::Strict,
        crate::redaction::Mode::Off,
    ] {
        let off = matches!(mode, crate::redaction::Mode::Off);
        let strict = matches!(mode, crate::redaction::Mode::Strict);
        let captured = Arc::new(Mutex::new(Vec::new()));
        let provider = ProtectedMeterProvider {
            inner: SdkMeterProvider::builder()
                .with_reader(
                    PeriodicReader::builder(Capture(captured.clone()))
                        .with_interval(Duration::from_secs(3600))
                        .build(),
                )
                .build(),
            policy: Policy::new(mode, "account_number").unwrap(),
            state: Default::default(),
        };
        let scope = InstrumentationScope::builder("scope-name-fixture")
            .with_version("scope-version-fixture")
            .with_schema_url("https://fixture.example/schema-secret")
            .with_attributes([
                KeyValue::new("email", "scope-builtin-secret"),
                KeyValue::new("account_number", "scope-custom-secret"),
            ])
            .build();
        provider
            .meter_with_scope(scope)
            .u64_counter("counter")
            .build()
            .add(1, &[]);
        provider.inner.force_flush().unwrap();
        provider.inner.shutdown().unwrap();
        let payload = captured.lock().unwrap().join("\n");
        assert!(payload.contains("counter"));
        for secret in [
            "scope-builtin-secret",
            "scope-custom-secret",
            "schema-secret",
        ] {
            assert_eq!(payload.contains(secret), off);
        }
        assert_eq!(payload.contains("scope-name-fixture"), !strict);
        assert_eq!(payload.contains("scope-version-fixture"), !strict);
    }
}
#[test]
fn repeated_original_series_fails_even_when_attribute_order_changes() {
    let policy = Policy::new(crate::redaction::Mode::Public, "").unwrap();
    let state = MetricState::default();
    let observations = Summing {
        groups: std::sync::Mutex::new(Vec::new()),
        originals: std::sync::Mutex::new(Vec::new()),
        policy: &policy,
        state: &state,
    };
    observations.observe(
        2_u64,
        &[
            KeyValue::new("email", "synthetic"),
            KeyValue::new("safe", true),
        ],
    );
    assert!(!state.failed());
    observations.observe(
        8_u64,
        &[
            KeyValue::new("safe", true),
            KeyValue::new("email", "synthetic"),
        ],
    );
    assert!(state.failed());
    let groups = observations.groups.into_inner().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].1, 2);
    assert_eq!(observations.originals.into_inner().unwrap().len(), 1);
}
#[test]
fn concurrent_observers_normalize_key_order_without_losing_sums() {
    let policy = Policy::new(crate::redaction::Mode::Public, "").unwrap();
    let state = MetricState::default();
    let observations = Summing {
        groups: std::sync::Mutex::new(Vec::new()),
        originals: std::sync::Mutex::new(Vec::new()),
        policy: &policy,
        state: &state,
    };
    std::thread::scope(|scope| {
        for index in 0..8 {
            let observations = &observations;
            scope.spawn(move || {
                for observation in 0..10 {
                    let mut labels = vec![
                        KeyValue::new("email", format!("synthetic-{index}-{observation}")),
                        KeyValue::new("safe", "same"),
                    ];
                    if index % 2 == 0 {
                        labels.reverse();
                    }
                    observations.observe(1_u64, &labels);
                }
            });
        }
    });
    assert!(!state.failed());
    let groups = observations.groups.into_inner().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].1, 80);
    assert_eq!(groups[0].0[0].key.as_str(), "email");
}
#[test]
fn duplicate_attribute_keys_fail_before_creating_groups() {
    let policy = Policy::new(crate::redaction::Mode::Off, "").unwrap();
    let state = MetricState::default();
    let observations = Summing {
        groups: std::sync::Mutex::new(Vec::new()),
        originals: std::sync::Mutex::new(Vec::new()),
        policy: &policy,
        state: &state,
    };
    observations.observe(
        1_u64,
        &[
            KeyValue::new("same", "a"),
            KeyValue::new("other", "b"),
            KeyValue::new("same", "c"),
        ],
    );
    assert!(state.failed());
    assert!(observations.groups.into_inner().unwrap().is_empty());
}
#[test]
fn observable_collection_limits_fail_before_growing_storage() {
    let policy = Policy::new(crate::redaction::Mode::Off, "").unwrap();
    let state = MetricState::default();
    let observations = Summing {
        groups: std::sync::Mutex::new(Vec::new()),
        originals: std::sync::Mutex::new(Vec::new()),
        policy: &policy,
        state: &state,
    };
    for index in 0..MAX_OBSERVABLE_GROUPS {
        observations.observe(1_u64, &[KeyValue::new("index", index as i64)]);
    }
    assert!(!state.failed());
    observations.observe(1, &[KeyValue::new("index", MAX_OBSERVABLE_GROUPS as i64)]);
    assert!(state.failed());
    assert_eq!(
        observations.groups.into_inner().unwrap().len(),
        MAX_OBSERVABLE_GROUPS
    );
    let key = "k";
    assert!(labels_within_budget(&[KeyValue::new(
        key,
        "x".repeat(MAX_OBSERVABLE_ATTRIBUTE_BYTES - 1)
    )]));
    assert!(!labels_within_budget(&[KeyValue::new(
        key,
        "x".repeat(MAX_OBSERVABLE_ATTRIBUTE_BYTES)
    )]));
    assert!(!labels_within_budget(&vec![
        KeyValue::new(key, 1);
        MAX_OBSERVABLE_ATTRIBUTES + 1
    ]));
    assert!(!labels_within_budget(&[KeyValue::new(
        key,
        opentelemetry::Value::Array(opentelemetry::Array::String(
            vec!["".into(); MAX_OBSERVABLE_ATTRIBUTE_BYTES]
        ))
    )]));
}
#[test]
fn reused_policy_does_not_share_provider_failure_state() {
    let policy = Policy::new(crate::redaction::Mode::Public, "").unwrap();
    let create = |policy| ProtectedMeterProvider {
        inner: SdkMeterProvider::builder()
            .with_reader(
                PeriodicReader::builder(Capture(Arc::new(Mutex::new(Vec::new()))))
                    .with_interval(Duration::from_secs(3600))
                    .build(),
            )
            .build(),
        policy,
        state: Default::default(),
    };
    let failing = create(policy.clone());
    let healthy = create(policy);
    let _bad = failing
        .meter("bad")
        .u64_observable_counter("overflow")
        .with_callback(|instrument| {
            instrument.observe(u64::MAX, &[]);
            instrument.observe(1, &[]);
        })
        .build();
    let _good = healthy
        .meter("good")
        .u64_observable_counter("healthy")
        .with_callback(|instrument| {
            instrument.observe(1, &[]);
        })
        .build();
    failing.inner.force_flush().unwrap();
    healthy.inner.force_flush().unwrap();
    assert!(failing.state.failed());
    assert!(!healthy.state.failed());
    failing.inner.force_flush().unwrap();
    assert!(failing.state.failed());
    assert!(!healthy.state.failed());
    failing.inner.shutdown().unwrap();
    healthy.inner.shutdown().unwrap();
}
#[test]
fn observable_sum_overflow_and_nonfinite_values_fail_without_wrapping() {
    let policy = Policy::new(crate::redaction::Mode::Public, "").unwrap();
    let state = MetricState::default();
    let observations = Summing {
        groups: std::sync::Mutex::new(Vec::new()),
        originals: std::sync::Mutex::new(Vec::new()),
        policy: &policy,
        state: &state,
    };
    observations.observe(u64::MAX, &[KeyValue::new("email", "first")]);
    observations.observe(1_u64, &[KeyValue::new("email", "second")]);
    assert!(state.failed());
    let policy = Policy::new(crate::redaction::Mode::Public, "").unwrap();
    let state = MetricState::default();
    let observations = Summing {
        groups: std::sync::Mutex::new(Vec::new()),
        originals: std::sync::Mutex::new(Vec::new()),
        policy: &policy,
        state: &state,
    };
    observations.observe(f64::NAN, &[]);
    assert!(state.failed());
    assert!(observations.groups.into_inner().unwrap().is_empty());
    assert_eq!(i64::MAX.checked_sum(1), None);
    assert_eq!(f64::MAX.checked_sum(f64::MAX), None);
}
use crate::redaction::Mode;
use opentelemetry_sdk::{
    error::OTelSdkResult,
    metrics::{PeriodicReader, Temporality, data::ResourceMetrics, exporter::PushMetricExporter},
};
use std::{sync::Mutex, time::Duration};
struct Capture(Arc<Mutex<Vec<String>>>);
type Values = std::collections::BTreeMap<String, Vec<i64>>;
struct CollisionCapture(Arc<Mutex<Values>>);
impl PushMetricExporter for CollisionCapture {
    async fn export(&self, metrics: &ResourceMetrics) -> OTelSdkResult {
        use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
        let mut values = Values::new();
        for scope in metrics.scope_metrics() {
            for metric in scope.metrics() {
                let points = match metric.data() {
                    AggregatedMetrics::I64(MetricData::Gauge(data)) => {
                        data.data_points().map(|p| p.value()).collect()
                    }
                    AggregatedMetrics::I64(MetricData::Sum(data)) => {
                        data.data_points().map(|p| p.value()).collect()
                    }
                    _ => vec![i64::MIN],
                };
                values.insert(metric.name().to_owned(), points);
            }
        }
        *self.0.lock().unwrap() = values;
        Ok(())
    }
    fn force_flush(&self) -> OTelSdkResult {
        Ok(())
    }
    fn shutdown_with_timeout(&self, _: Duration) -> OTelSdkResult {
        Ok(())
    }
    fn temporality(&self) -> Temporality {
        Temporality::Cumulative
    }
}
#[test]
fn sdk_cardinality_overflow_retains_counter_total_across_collections() {
    let captured = Arc::new(Mutex::new(Values::new()));
    let provider = ProtectedMeterProvider {
        inner: bounded_meter_builder()
            .unwrap()
            .with_reader(
                PeriodicReader::builder(CollisionCapture(captured.clone()))
                    .with_interval(Duration::from_secs(3600))
                    .build(),
            )
            .build(),
        policy: Policy::new(Mode::Off, "").unwrap(),
        state: Default::default(),
    };
    let counter = provider
        .meter("capacity")
        .i64_up_down_counter("counter")
        .build();
    let count = MAX_STREAM_SERIES * 2;
    for index in 0..count {
        counter.add(1, &[KeyValue::new("series", index as i64)]);
    }
    for _ in 0..2 {
        provider.inner.force_flush().unwrap();
        let values = captured.lock().unwrap();
        let points = &values["counter"];
        assert!(points.len() <= MAX_STREAM_SERIES + 1);
        assert_eq!(points.iter().sum::<i64>(), count as i64);
    }
    assert!(!provider.state.failed());
    provider.inner.shutdown().unwrap();
}

#[test]
fn gauge_and_observable_collisions_have_explicit_sdk_semantics() {
    for mode in [Mode::Public, Mode::Strict, Mode::Off] {
        let off = matches!(mode, Mode::Off);
        let captured = Arc::new(Mutex::new(Values::new()));
        let provider = ProtectedMeterProvider {
            inner: SdkMeterProvider::builder()
                .with_reader(
                    PeriodicReader::builder(CollisionCapture(captured.clone()))
                        .with_interval(Duration::from_secs(3600))
                        .build(),
                )
                .build(),
            policy: Policy::new(mode, "").unwrap(),
            state: Default::default(),
        };
        let meter = provider.meter("collision-fixture");
        let gauge = meter.i64_gauge("gauge").build();
        let changes = meter.i64_up_down_counter("changes").build();
        for (label, value) in [("first-secret", 2), ("second-secret", 8)] {
            let attributes = [KeyValue::new("email", label)];
            gauge.record(value, &attributes);
            changes.add(value, &attributes);
        }
        let _observable = meter
            .i64_observable_gauge("observable_gauge")
            .with_callback(|instrument| {
                instrument.observe(2, &[KeyValue::new("email", "first-secret")]);
                instrument.observe(8, &[KeyValue::new("email", "second-secret")]);
            })
            .build();
        let _observable_changes = meter
            .i64_observable_up_down_counter("observable_changes")
            .with_callback(|instrument| {
                instrument.observe(2, &[KeyValue::new("email", "first-secret")]);
            })
            .with_callback(|instrument| {
                instrument.observe(8, &[KeyValue::new("email", "second-secret")]);
            })
            .build();
        provider.inner.force_flush().unwrap();
        let mut values = captured.lock().unwrap().clone();
        assert_eq!(values.len(), 4);
        for points in values.values_mut() {
            points.sort();
        }
        assert_eq!(
            values.get("gauge"),
            Some(&if off { vec![2, 8] } else { vec![8] })
        );
        assert_eq!(
            values.get("changes"),
            Some(&if off { vec![2, 8] } else { vec![10] })
        );
        assert_eq!(
            values.get("observable_gauge"),
            Some(&if off { vec![2, 8] } else { vec![8] })
        );
        assert_eq!(
            values.get("observable_changes"),
            Some(&if off { vec![2, 8] } else { vec![10] })
        );
        // A second collection must not add absolute observable values again.
        provider.inner.force_flush().unwrap();
        let mut second = captured.lock().unwrap().clone();
        for points in second.values_mut() {
            points.sort();
        }
        assert_eq!(second, values);
        provider.inner.shutdown().unwrap();
    }
}
#[derive(Debug, Default, PartialEq)]
struct Totals {
    counter: u64,
    counter_series: usize,
    histogram_sum: u64,
    histogram_count: u64,
    histogram_series: usize,
    buckets: Vec<u64>,
    unexpected_data: bool,
}
struct NumericCapture(Arc<Mutex<Totals>>);
impl PushMetricExporter for NumericCapture {
    async fn export(&self, metrics: &ResourceMetrics) -> OTelSdkResult {
        use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
        let mut totals = Totals::default();
        for scope in metrics.scope_metrics() {
            for metric in scope.metrics() {
                if let AggregatedMetrics::U64(data) = metric.data() {
                    match data {
                        MetricData::Sum(sum) => {
                            for point in sum.data_points() {
                                totals.counter += point.value();
                                totals.counter_series += 1;
                            }
                        }
                        MetricData::Histogram(histogram) => {
                            for point in histogram.data_points() {
                                totals.histogram_sum += point.sum();
                                totals.histogram_count += point.count();
                                totals.histogram_series += 1;
                                totals.unexpected_data |=
                                    point.bounds().collect::<Vec<_>>() != vec![5.0];
                                let buckets = point.bucket_counts().collect::<Vec<_>>();
                                if totals.buckets.is_empty() {
                                    totals.buckets.resize(buckets.len(), 0);
                                }
                                totals.unexpected_data |= totals.buckets.len() != buckets.len();
                                for (total, count) in totals.buckets.iter_mut().zip(buckets) {
                                    *total += count;
                                }
                            }
                        }
                        _ => totals.unexpected_data = true,
                    }
                } else {
                    totals.unexpected_data = true;
                }
            }
        }
        *self.0.lock().unwrap() = totals;
        Ok(())
    }
    fn force_flush(&self) -> OTelSdkResult {
        Ok(())
    }
    fn shutdown_with_timeout(&self, _: Duration) -> OTelSdkResult {
        Ok(())
    }
    fn temporality(&self) -> Temporality {
        Temporality::Cumulative
    }
}
#[derive(Debug, Default)]
struct ObservableSnapshot {
    total: u64,
    series: usize,
    unexpected: bool,
}
struct ObservableCapture {
    snapshots: Arc<Mutex<Vec<ObservableSnapshot>>>,
    temporality: Temporality,
}
impl PushMetricExporter for ObservableCapture {
    async fn export(&self, metrics: &ResourceMetrics) -> OTelSdkResult {
        use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
        let mut snapshot = ObservableSnapshot::default();
        for scope in metrics.scope_metrics() {
            for metric in scope.metrics() {
                if metric.name() == "heartbeat" {
                    continue;
                }
                if let AggregatedMetrics::U64(MetricData::Sum(sum)) = metric.data() {
                    snapshot.unexpected |= metric.name() != "observable-counter"
                        || sum.temporality() != self.temporality
                        || !sum.is_monotonic();
                    for point in sum.data_points() {
                        snapshot.total += point.value();
                        snapshot.series += 1;
                    }
                } else {
                    snapshot.unexpected = true;
                }
            }
        }
        self.snapshots.lock().unwrap().push(snapshot);
        Ok(())
    }
    fn force_flush(&self) -> OTelSdkResult {
        Ok(())
    }
    fn shutdown_with_timeout(&self, _: Duration) -> OTelSdkResult {
        Ok(())
    }
    fn temporality(&self) -> Temporality {
        self.temporality
    }
}
#[test]
fn observable_counter_repeated_absolute_values_do_not_accumulate_again() {
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    for temporality in [Temporality::Cumulative, Temporality::Delta] {
        for mode in [Mode::Off, Mode::Public, Mode::Strict] {
            let strict = matches!(mode, Mode::Strict);
            let mode_name = match mode {
                Mode::Off => "off",
                Mode::Public => "public",
                Mode::Strict => "strict",
            };
            let label_count = Arc::new(AtomicUsize::new(1024));
            let value = Arc::new(AtomicU64::new(2));
            let callbacks = Arc::new(AtomicUsize::new(0));
            let snapshots = Arc::new(Mutex::new(Vec::new()));
            let provider = ProtectedMeterProvider {
                inner: bounded_meter_builder()
                    .unwrap()
                    .with_reader(
                        PeriodicReader::builder(ObservableCapture {
                            snapshots: snapshots.clone(),
                            temporality,
                        })
                        .with_interval(Duration::from_secs(3600))
                        .build(),
                    )
                    .build(),
                policy: Policy::new(mode, "").unwrap(),
                state: Default::default(),
            };
            let meter = provider.meter("observable-interval-fixture");
            let callback_value = value.clone();
            let callback_count = callbacks.clone();
            let callback_labels = label_count.clone();
            let _counter = meter
                .u64_observable_counter("observable-counter")
                .with_callback(move |instrument| {
                    callback_count.fetch_add(1, Ordering::SeqCst);
                    let absolute = callback_value.load(Ordering::SeqCst);
                    for index in 0..callback_labels.load(Ordering::SeqCst) {
                        instrument.observe(
                            absolute,
                            &[KeyValue::new("identity", format!("label-{index}"))],
                        );
                    }
                })
                .build();
            // Force an export even if an unchanged delta observable has no points.
            let heartbeat = meter.u64_counter("heartbeat").build();
            for (interval, absolute) in [2, 2, 5].into_iter().enumerate() {
                value.store(absolute, Ordering::SeqCst);
                heartbeat.add(1, &[]);
                provider.inner.force_flush().unwrap();
                assert_eq!(callbacks.load(Ordering::SeqCst), interval + 1);
                let captured = snapshots.lock().unwrap();
                assert_eq!(captured.len(), interval + 1);
                let actual = captured.last().unwrap();
                let expected = if temporality == Temporality::Cumulative {
                    absolute * 1024
                } else {
                    [2048, 0, 3072][interval]
                };
                assert_eq!(actual.total, expected);
                assert!(!actual.unexpected, "{actual:?}");
                assert!(actual.series <= 1024);
                if expected != 0 {
                    assert_eq!(actual.series, if strict { 1 } else { 1024 });
                }
                assert!(!provider.state.failed());
            }
            // The adapter budget is for original labels, even when strict redaction merges them.
            label_count.store(1025, Ordering::SeqCst);
            heartbeat.add(1, &[]);
            provider.inner.force_flush().unwrap();
            assert!(provider.state.failed());
            assert_eq!(callbacks.load(Ordering::SeqCst), 4);
            {
                let captured = snapshots.lock().unwrap();
                assert_eq!(captured.len(), 4);
                assert_eq!(captured.last().unwrap().total, 0);
                assert_eq!(captured.last().unwrap().series, 0);
            }
            provider.inner.force_flush().unwrap();
            assert!(provider.state.failed());
            assert_eq!(callbacks.load(Ordering::SeqCst), 4);
            eprintln!(
                "OTLP_OBSERVABLE_PASS temporality={temporality:?} mode={mode_name} absolute=2,2,5 original-labels=1024 overflow=1025 rejected-no-partial callback-skipped-after-failure"
            );
            provider.inner.shutdown().unwrap();
        }
    }
}
#[test]
fn failed_sum_callback_skips_later_callbacks_in_same_collection() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for mode in [Mode::Off, Mode::Public, Mode::Strict] {
        let provider = ProtectedMeterProvider {
            inner: bounded_meter_builder()
                .unwrap()
                .with_reader(
                    PeriodicReader::builder(Capture(Arc::new(Mutex::new(Vec::new()))))
                        .with_interval(Duration::from_secs(3600))
                        .build(),
                )
                .build(),
            policy: Policy::new(mode, "").unwrap(),
            state: Default::default(),
        };
        let first = Arc::new(AtomicUsize::new(0));
        let second = Arc::new(AtomicUsize::new(0));
        let first_calls = first.clone();
        let second_calls = second.clone();
        let _counter = provider
            .meter("failed-callback-fixture")
            .u64_observable_counter("counter")
            .with_callback(move |instrument| {
                first_calls.fetch_add(1, Ordering::SeqCst);
                instrument.observe(1, &[KeyValue::new("identity", "same")]);
                instrument.observe(2, &[KeyValue::new("identity", "same")]);
            })
            .with_callback(move |_| {
                second_calls.fetch_add(1, Ordering::SeqCst);
            })
            .build();
        provider.inner.force_flush().unwrap();
        assert!(provider.state.failed());
        assert_eq!(first.load(Ordering::SeqCst), 1);
        assert_eq!(second.load(Ordering::SeqCst), 0);
        provider.inner.force_flush().unwrap();
        provider.inner.shutdown().unwrap();
        assert_eq!(first.load(Ordering::SeqCst), 1);
        assert_eq!(second.load(Ordering::SeqCst), 0);
    }
}
#[derive(Debug, Default)]
struct UpDownSnapshot {
    metric_count: usize,
    signed: i64,
    fraction: f64,
    signed_series: usize,
    fraction_series: usize,
    unexpected: bool,
}
struct UpDownCapture {
    snapshots: Arc<Mutex<Vec<UpDownSnapshot>>>,
    requested: Temporality,
    strict: bool,
}
impl PushMetricExporter for UpDownCapture {
    async fn export(&self, metrics: &ResourceMetrics) -> OTelSdkResult {
        use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
        let mut snapshot = UpDownSnapshot::default();
        let valid_attribute = |attribute: &KeyValue| {
            attribute.key.as_str() == "identity"
                && match &attribute.value {
                    opentelemetry::Value::String(value) if self.strict => {
                        value.as_str() == "[REDACTED]"
                    }
                    opentelemetry::Value::String(value) => value
                        .as_str()
                        .strip_prefix("label-")
                        .and_then(|suffix| suffix.parse::<usize>().ok())
                        .is_some_and(|index| index < 1024),
                    _ => false,
                }
        };
        for scope in metrics.scope_metrics() {
            for metric in scope.metrics() {
                snapshot.metric_count += 1;
                match metric.data() {
                    AggregatedMetrics::I64(MetricData::Sum(sum)) => {
                        snapshot.unexpected |= metric.name() != "signed"
                            || sum.temporality() != Temporality::Cumulative
                            || sum.is_monotonic();
                        for point in sum.data_points() {
                            snapshot.unexpected |= point.attributes().count() != 1
                                || !point.attributes().all(valid_attribute);
                            snapshot.signed += point.value();
                            snapshot.signed_series += 1;
                        }
                    }
                    AggregatedMetrics::F64(MetricData::Sum(sum)) => {
                        snapshot.unexpected |= metric.name() != "fraction"
                            || sum.temporality() != Temporality::Cumulative
                            || sum.is_monotonic();
                        for point in sum.data_points() {
                            snapshot.unexpected |= !point.value().is_finite()
                                || point.attributes().count() != 1
                                || !point.attributes().all(valid_attribute);
                            snapshot.fraction += point.value();
                            snapshot.fraction_series += 1;
                        }
                    }
                    _ => snapshot.unexpected = true,
                }
            }
        }
        self.snapshots.lock().unwrap().push(snapshot);
        Ok(())
    }
    fn force_flush(&self) -> OTelSdkResult {
        Ok(())
    }
    fn shutdown_with_timeout(&self, _: Duration) -> OTelSdkResult {
        Ok(())
    }
    fn temporality(&self) -> Temporality {
        self.requested
    }
}
#[test]
fn observable_up_down_preserves_negative_absolute_values_for_delta_request() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for requested in [Temporality::Cumulative, Temporality::Delta] {
        for mode in [Mode::Off, Mode::Public, Mode::Strict] {
            let strict = matches!(mode, Mode::Strict);
            let mode_name = match mode {
                Mode::Off => "off",
                Mode::Public => "public",
                Mode::Strict => "strict",
            };
            let snapshots = Arc::new(Mutex::new(Vec::new()));
            let values = Arc::new(Mutex::new((2_i64, 0.5_f64)));
            let signed_calls = Arc::new(AtomicUsize::new(0));
            let fraction_calls = Arc::new(AtomicUsize::new(0));
            let provider = ProtectedMeterProvider {
                inner: bounded_meter_builder()
                    .unwrap()
                    .with_reader(
                        PeriodicReader::builder(UpDownCapture {
                            snapshots: snapshots.clone(),
                            requested,
                            strict,
                        })
                        .with_interval(Duration::from_secs(3600))
                        .build(),
                    )
                    .build(),
                policy: Policy::new(mode, "").unwrap(),
                state: Default::default(),
            };
            let meter = provider.meter("up-down-interval-fixture");
            let signed_values = values.clone();
            let signed_count = signed_calls.clone();
            let _signed = meter
                .i64_observable_up_down_counter("signed")
                .with_callback(move |instrument| {
                    signed_count.fetch_add(1, Ordering::SeqCst);
                    let value = signed_values.lock().unwrap().0;
                    for index in 0..1024 {
                        instrument.observe(
                            value,
                            &[KeyValue::new("identity", format!("label-{index}"))],
                        );
                    }
                })
                .build();
            let fraction_values = values.clone();
            let fraction_count = fraction_calls.clone();
            let _fraction = meter
                .f64_observable_up_down_counter("fraction")
                .with_callback(move |instrument| {
                    fraction_count.fetch_add(1, Ordering::SeqCst);
                    let value = fraction_values.lock().unwrap().1;
                    for index in 0..1024 {
                        instrument.observe(
                            value,
                            &[KeyValue::new("identity", format!("label-{index}"))],
                        );
                    }
                })
                .build();
            for (interval, value) in [(2, 0.5), (-3, -1.5), (-3, -1.5)].into_iter().enumerate() {
                *values.lock().unwrap() = value;
                provider.inner.force_flush().unwrap();
                assert_eq!(signed_calls.load(Ordering::SeqCst), interval + 1);
                assert_eq!(fraction_calls.load(Ordering::SeqCst), interval + 1);
                let captured = snapshots.lock().unwrap();
                assert_eq!(captured.len(), interval + 1);
                let actual = captured.last().unwrap();
                assert_eq!(actual.metric_count, 2);
                assert_eq!(actual.signed, value.0 * 1024);
                assert_eq!(actual.fraction, value.1 * 1024.0);
                assert_eq!(actual.signed_series, if strict { 1 } else { 1024 });
                assert_eq!(actual.fraction_series, if strict { 1 } else { 1024 });
                assert!(!actual.unexpected, "{actual:?}");
                assert!(!provider.state.failed());
            }
            eprintln!(
                "OTLP_UPDOWN_PASS requested={requested:?} actual=Cumulative mode={mode_name} signed=2048,-3072,-3072 fraction=512,-1536,-1536 policy-labels no-overflow"
            );
            provider.inner.shutdown().unwrap();
        }
    }
}
#[test]
fn actual_float_observable_callbacks_reject_nonfinite_measurements() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for mode in [Mode::Off, Mode::Public, Mode::Strict] {
        let mode_name = match mode {
            Mode::Off => "off",
            Mode::Public => "public",
            Mode::Strict => "strict",
        };
        for (invalid_name, invalid) in [
            ("nan", f64::NAN),
            ("positive-infinity", f64::INFINITY),
            ("negative-infinity", f64::NEG_INFINITY),
        ] {
            for up_down in [false, true] {
                let captured = Arc::new(Mutex::new(Vec::new()));
                let calls = Arc::new(AtomicUsize::new(0));
                let provider = ProtectedMeterProvider {
                    inner: bounded_meter_builder()
                        .unwrap()
                        .with_reader(
                            PeriodicReader::builder(Capture(captured.clone()))
                                .with_interval(Duration::from_secs(3600))
                                .build(),
                        )
                        .build(),
                    policy: Policy::new(mode.clone(), "").unwrap(),
                    state: Default::default(),
                };
                let meter = provider.meter("invalid-number-fixture");
                let callback_calls = calls.clone();
                let callback = move |instrument: &dyn AsyncInstrument<f64>| {
                    callback_calls.fetch_add(1, Ordering::SeqCst);
                    instrument.observe(1.0, &[KeyValue::new("identity", "valid")]);
                    instrument.observe(invalid, &[KeyValue::new("identity", "invalid")]);
                };
                if up_down {
                    let _instrument = meter
                        .f64_observable_up_down_counter("invalid")
                        .with_callback(callback)
                        .build();
                } else {
                    let _instrument = meter
                        .f64_observable_counter("invalid")
                        .with_callback(callback)
                        .build();
                }
                provider.inner.force_flush().unwrap();
                assert!(provider.state.failed());
                assert_eq!(calls.load(Ordering::SeqCst), 1);
                assert!(
                    captured.lock().unwrap().is_empty(),
                    "invalid collection leaked earlier valid observation"
                );
                provider.inner.force_flush().unwrap();
                provider.inner.shutdown().unwrap();
                assert_eq!(calls.load(Ordering::SeqCst), 1);
                eprintln!(
                    "OTLP_INVALID_NUMBER_PASS mode={mode_name} up-down={up_down} input={invalid_name} sticky-failure no-export no-later-callback"
                );
            }
        }
    }
}
struct ExtremeCapture(Arc<Mutex<Vec<f64>>>);
impl PushMetricExporter for ExtremeCapture {
    async fn export(&self, metrics: &ResourceMetrics) -> OTelSdkResult {
        use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
        for scope in metrics.scope_metrics() {
            for metric in scope.metrics() {
                if let AggregatedMetrics::F64(MetricData::Sum(sum)) = metric.data() {
                    assert_eq!(sum.temporality(), Temporality::Cumulative);
                    for point in sum.data_points() {
                        self.0.lock().unwrap().push(point.value());
                    }
                } else {
                    panic!("unexpected extreme-value metric data");
                }
            }
        }
        Ok(())
    }
    fn force_flush(&self) -> OTelSdkResult {
        Ok(())
    }
    fn shutdown_with_timeout(&self, _: Duration) -> OTelSdkResult {
        Ok(())
    }
    fn temporality(&self) -> Temporality {
        Temporality::Cumulative
    }
}
#[test]
fn masked_float_callback_sum_overflow_is_detected_before_export() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for mode in [Mode::Off, Mode::Public, Mode::Strict] {
        let off = matches!(mode, Mode::Off);
        let mode_name = match mode {
            Mode::Off => "off",
            Mode::Public => "public",
            Mode::Strict => "strict",
        };
        for up_down in [false, true] {
            let captured = Arc::new(Mutex::new(Vec::new()));
            let calls = Arc::new(AtomicUsize::new(0));
            let provider = ProtectedMeterProvider {
                inner: bounded_meter_builder()
                    .unwrap()
                    .with_reader(
                        PeriodicReader::builder(ExtremeCapture(captured.clone()))
                            .with_interval(Duration::from_secs(3600))
                            .build(),
                    )
                    .build(),
                policy: Policy::new(mode.clone(), "").unwrap(),
                state: Default::default(),
            };
            let callback_calls = calls.clone();
            let callback = move |instrument: &dyn AsyncInstrument<f64>| {
                callback_calls.fetch_add(1, Ordering::SeqCst);
                instrument.observe(f64::MAX, &[KeyValue::new("email", "first@example.invalid")]);
                instrument.observe(
                    f64::MAX,
                    &[KeyValue::new("email", "second@example.invalid")],
                );
            };
            let meter = provider.meter("masked-overflow-fixture");
            if up_down {
                let _instrument = meter
                    .f64_observable_up_down_counter("extreme")
                    .with_callback(callback)
                    .build();
            } else {
                let _instrument = meter
                    .f64_observable_counter("extreme")
                    .with_callback(callback)
                    .build();
            }
            provider.inner.force_flush().unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(provider.state.failed(), !off);
            if off {
                assert_eq!(*captured.lock().unwrap(), vec![f64::MAX, f64::MAX]);
            } else {
                assert!(captured.lock().unwrap().is_empty());
                provider.inner.force_flush().unwrap();
                assert_eq!(calls.load(Ordering::SeqCst), 1);
            }
            eprintln!(
                "OTLP_MASKED_OVERFLOW_PASS mode={mode_name} up-down={up_down} separate-finite-series={off} failed-before-export={}",
                !off
            );
            provider.inner.shutdown().unwrap();
        }
    }
}
#[derive(Debug, Default)]
struct FloatTotals {
    metric_count: usize,
    minimum: Option<f64>,
    maximum: Option<f64>,
    counter: f64,
    histogram_sum: f64,
    histogram_count: u64,
    counter_series: usize,
    histogram_series: usize,
    counter_overflow: usize,
    histogram_overflow: usize,
    buckets: Vec<u64>,
    unexpected: bool,
}
struct FloatCapture {
    totals: Arc<Mutex<FloatTotals>>,
    temporality: Temporality,
}
impl PushMetricExporter for FloatCapture {
    async fn export(&self, metrics: &ResourceMetrics) -> OTelSdkResult {
        use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
        let mut totals = FloatTotals::default();
        let overflow = |attribute: &KeyValue| {
            attribute.key.as_str() == "otel.metric.overflow"
                && attribute.value == opentelemetry::Value::Bool(true)
        };
        for scope in metrics.scope_metrics() {
            for metric in scope.metrics() {
                totals.metric_count += 1;
                match metric.data() {
                    AggregatedMetrics::F64(MetricData::Sum(sum)) => {
                        totals.unexpected |= sum.temporality() != self.temporality
                            || !sum.is_monotonic()
                            || metric.name() != "float-counter";
                        for point in sum.data_points() {
                            totals.unexpected |= !point.value().is_finite();
                            totals.counter += point.value();
                            totals.counter_series += 1;
                            totals.counter_overflow +=
                                usize::from(point.attributes().any(overflow));
                        }
                    }
                    AggregatedMetrics::F64(MetricData::Histogram(histogram)) => {
                        totals.unexpected |= histogram.temporality() != self.temporality
                            || metric.name() != "float-histogram";
                        for point in histogram.data_points() {
                            totals.unexpected |= !point.sum().is_finite()
                                || point.min().is_none()
                                || point.max().is_none();
                            if let Some(value) = point.min() {
                                totals.unexpected |= !value.is_finite();
                                totals.minimum = Some(
                                    totals.minimum.map_or(value, |current| current.min(value)),
                                );
                            }
                            if let Some(value) = point.max() {
                                totals.unexpected |= !value.is_finite();
                                totals.maximum = Some(
                                    totals.maximum.map_or(value, |current| current.max(value)),
                                );
                            }
                            totals.histogram_sum += point.sum();
                            totals.histogram_count += point.count();
                            totals.histogram_series += 1;
                            totals.histogram_overflow +=
                                usize::from(point.attributes().any(overflow));
                            totals.unexpected |= point.bounds().collect::<Vec<_>>() != vec![1.0];
                            let buckets = point.bucket_counts().collect::<Vec<_>>();
                            if totals.buckets.is_empty() {
                                totals.buckets.resize(buckets.len(), 0);
                            }
                            totals.unexpected |= totals.buckets.len() != buckets.len();
                            for (total, count) in totals.buckets.iter_mut().zip(buckets) {
                                *total += count;
                            }
                        }
                    }
                    _ => totals.unexpected = true,
                }
            }
        }
        *self.totals.lock().unwrap() = totals;
        Ok(())
    }
    fn force_flush(&self) -> OTelSdkResult {
        Ok(())
    }
    fn shutdown_with_timeout(&self, _: Duration) -> OTelSdkResult {
        Ok(())
    }
    fn temporality(&self) -> Temporality {
        self.temporality
    }
}
#[test]
fn float_capacity_preserves_binary_fraction_totals_in_both_temporalities() {
    for temporality in [Temporality::Cumulative, Temporality::Delta] {
        for mode in [Mode::Off, Mode::Public, Mode::Strict] {
            let strict = matches!(mode, Mode::Strict);
            let mode_name = match mode {
                Mode::Off => "off",
                Mode::Public => "public",
                Mode::Strict => "strict",
            };
            let totals = Arc::new(Mutex::new(FloatTotals::default()));
            let provider = ProtectedMeterProvider {
                inner: bounded_meter_builder()
                    .unwrap()
                    .with_reader(
                        PeriodicReader::builder(FloatCapture {
                            totals: totals.clone(),
                            temporality,
                        })
                        .with_interval(Duration::from_secs(3600))
                        .build(),
                    )
                    .build(),
                policy: Policy::new(mode, "").unwrap(),
                state: Default::default(),
            };
            let meter = provider.meter("float-capacity-fixture");
            let counter = meter.f64_counter("float-counter").build();
            let histogram = meter
                .f64_histogram("float-histogram")
                .with_boundaries(vec![1.0])
                .build();
            // Binary fractions have exact sums here; no broad rounding tolerance can hide dropped values.
            for (interval, value) in [0.5, 1.5].into_iter().enumerate() {
                for index in 0..2048 {
                    let labels = [KeyValue::new("identity", format!("label-{index}"))];
                    counter.add(value, &labels);
                    histogram.record(value, &labels);
                }
                provider.inner.force_flush().unwrap();
                let actual = totals.lock().unwrap();
                let cumulative_second = interval == 1 && temporality == Temporality::Cumulative;
                let expected_sum = if cumulative_second {
                    4096.0
                } else {
                    value * 2048.0
                };
                assert_eq!(actual.metric_count, 2);
                assert_eq!(
                    actual.minimum,
                    Some(if cumulative_second { 0.5 } else { value })
                );
                assert_eq!(actual.maximum, Some(value));
                assert_eq!(actual.counter, expected_sum);
                assert_eq!(actual.histogram_sum, expected_sum);
                assert_eq!(
                    actual.histogram_count,
                    if cumulative_second { 4096 } else { 2048 }
                );
                assert_eq!(
                    actual.buckets,
                    if interval == 0 {
                        vec![2048, 0]
                    } else if cumulative_second {
                        vec![2048, 2048]
                    } else {
                        vec![0, 2048]
                    }
                );
                assert!(!actual.unexpected, "{actual:?}");
                assert!(actual.counter_series > 0 && actual.counter_series <= 1025);
                assert!(actual.histogram_series > 0 && actual.histogram_series <= 1025);
                assert_eq!(actual.counter_overflow, usize::from(!strict));
                assert_eq!(actual.histogram_overflow, usize::from(!strict));
                if strict {
                    assert_eq!(actual.counter_series, 1);
                    assert_eq!(actual.histogram_series, 1);
                }
            }
            eprintln!(
                "OTLP_F64_PASS temporality={temporality:?} mode={mode_name} intervals=2 exact-binary-fractions actual-overflow min-max"
            );
            assert!(!provider.state.failed());
            provider.inner.shutdown().unwrap();
        }
    }
}
#[derive(Default)]
struct DeltaProperties {
    non_delta: bool,
    sum_overflow: usize,
    histogram_overflow: usize,
}
struct DeltaNumericCapture {
    numeric: NumericCapture,
    properties: Arc<Mutex<DeltaProperties>>,
}
impl PushMetricExporter for DeltaNumericCapture {
    async fn export(&self, metrics: &ResourceMetrics) -> OTelSdkResult {
        use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
        let mut properties = DeltaProperties::default();
        let overflow = |attribute: &KeyValue| {
            attribute.key.as_str() == "otel.metric.overflow"
                && attribute.value == opentelemetry::Value::Bool(true)
        };
        for scope in metrics.scope_metrics() {
            for metric in scope.metrics() {
                if let AggregatedMetrics::U64(data) = metric.data() {
                    match data {
                        MetricData::Sum(sum) => {
                            properties.non_delta |=
                                !matches!(sum.temporality(), Temporality::Delta);
                            properties.sum_overflow += sum
                                .data_points()
                                .filter(|point| point.attributes().any(overflow))
                                .count();
                        }
                        MetricData::Histogram(histogram) => {
                            properties.non_delta |=
                                !matches!(histogram.temporality(), Temporality::Delta);
                            properties.histogram_overflow += histogram
                                .data_points()
                                .filter(|point| point.attributes().any(overflow))
                                .count();
                        }
                        _ => properties.non_delta = true,
                    }
                } else {
                    properties.non_delta = true;
                }
            }
        }
        *self.properties.lock().unwrap() = properties;
        self.numeric.export(metrics).await
    }
    fn force_flush(&self) -> OTelSdkResult {
        self.numeric.force_flush()
    }
    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.numeric.shutdown_with_timeout(timeout)
    }
    fn temporality(&self) -> Temporality {
        Temporality::Delta
    }
}
#[test]
fn bounded_delta_view_resets_interval_totals_without_losing_overflow() {
    for mode in [Mode::Off, Mode::Public, Mode::Strict] {
        let strict = matches!(mode, Mode::Strict);
        let mode_name = match mode {
            Mode::Off => "off",
            Mode::Public => "public",
            Mode::Strict => "strict",
        };
        let totals = Arc::new(Mutex::new(Totals::default()));
        let properties = Arc::new(Mutex::new(DeltaProperties::default()));
        let provider = ProtectedMeterProvider {
            inner: bounded_meter_builder()
                .unwrap()
                .with_reader(
                    PeriodicReader::builder(DeltaNumericCapture {
                        numeric: NumericCapture(totals.clone()),
                        properties: properties.clone(),
                    })
                    .with_interval(Duration::from_secs(3600))
                    .build(),
                )
                .build(),
            policy: Policy::new(mode, "").unwrap(),
            state: Default::default(),
        };
        let meter = provider.meter("delta-capacity-fixture");
        let counter = meter.u64_counter("counter").build();
        let histogram = meter
            .u64_histogram("histogram")
            .with_boundaries(vec![5.0])
            .build();
        for value in [2, 8] {
            for index in 0..2048 {
                let labels = [KeyValue::new("identity", format!("label-{index}"))];
                counter.add(value, &labels);
                histogram.record(value, &labels);
            }
            provider.inner.force_flush().unwrap();
            let actual = totals.lock().unwrap();
            assert_eq!(actual.counter, value * 2048);
            assert_eq!(actual.histogram_sum, value * 2048);
            assert_eq!(actual.histogram_count, 2048);
            assert_eq!(
                actual.buckets,
                if value == 2 {
                    vec![2048, 0]
                } else {
                    vec![0, 2048]
                }
            );
            assert!(!actual.unexpected_data);
            assert!(actual.counter_series > 0 && actual.counter_series <= 1025);
            assert!(actual.histogram_series > 0 && actual.histogram_series <= 1025);
            let exported = properties.lock().unwrap();
            assert!(!exported.non_delta);
            assert_eq!(exported.sum_overflow, usize::from(!strict));
            assert_eq!(exported.histogram_overflow, usize::from(!strict));
            if strict {
                assert_eq!(actual.counter_series, 1);
                assert_eq!(actual.histogram_series, 1);
            }
        }
        eprintln!(
            "OTLP_DELTA_PASS mode={mode_name} intervals=2 counter=4096,16384 histogram-count=2048,2048 actual-temporality actual-overflow"
        );
        assert!(!provider.state.failed());
        provider.inner.shutdown().unwrap();
    }
}
#[test]
fn bounded_view_preserves_histogram_boundaries_and_overflow_totals() {
    for mode in [Mode::Off, Mode::Public, Mode::Strict] {
        let totals = Arc::new(Mutex::new(Totals::default()));
        let provider = ProtectedMeterProvider {
            inner: bounded_meter_builder()
                .unwrap()
                .with_reader(
                    PeriodicReader::builder(NumericCapture(totals.clone()))
                        .with_interval(Duration::from_secs(3600))
                        .build(),
                )
                .build(),
            policy: Policy::new(mode.clone(), "").unwrap(),
            state: Default::default(),
        };
        let meter = provider.meter("bounded-histogram");
        let counter = meter.u64_counter("counter").build();
        let histogram = meter
            .u64_histogram("histogram")
            .with_boundaries(vec![5.0])
            .build();
        let count = MAX_STREAM_SERIES * 2;
        for index in 0..count {
            let labels = [KeyValue::new("series", index as i64)];
            counter.add(2, &labels);
            histogram.record(2, &labels);
        }
        for _ in 0..2 {
            provider.inner.force_flush().unwrap();
            let totals = totals.lock().unwrap();
            assert!(!totals.unexpected_data);
            assert_eq!(totals.counter, (count * 2) as u64);
            assert_eq!(totals.histogram_sum, (count * 2) as u64);
            assert_eq!(totals.histogram_count, count as u64);
            assert_eq!(totals.buckets, vec![count as u64, 0]);
            assert!(totals.counter_series <= MAX_STREAM_SERIES + 1);
            assert!(totals.histogram_series <= MAX_STREAM_SERIES + 1);
            if matches!(mode, Mode::Strict) {
                assert_eq!(totals.histogram_series, 1);
                assert_eq!(totals.counter_series, 1);
            }
        }
        assert!(!provider.state.failed());
        provider.inner.shutdown().unwrap();
    }
}

#[test]
fn masking_merges_series_without_losing_counter_or_histogram_totals() {
    for mode in [Mode::Public, Mode::Strict, Mode::Off] {
        let off = matches!(mode, Mode::Off);
        let totals = Arc::new(Mutex::new(Totals::default()));
        let provider = ProtectedMeterProvider {
            inner: bounded_meter_builder()
                .unwrap()
                .with_reader(
                    PeriodicReader::builder(NumericCapture(totals.clone()))
                        .with_interval(Duration::from_secs(3600))
                        .build(),
                )
                .build(),
            policy: Policy::new(mode, "").unwrap(),
            state: Default::default(),
        };
        let meter = provider.meter("numeric-fixture");
        let counter = meter
            .u64_counter("counter")
            .with_unit("runs")
            .with_description("fixture counter")
            .build();
        let histogram = meter
            .u64_histogram("histogram")
            .with_boundaries(vec![5.0])
            .build();
        for (label, value) in [("first-secret", 2), ("second-secret", 8)] {
            let labels = [KeyValue::new("email", label)];
            counter.add(value, &labels);
            histogram.record(value, &labels);
        }
        provider.inner.force_flush().unwrap();
        assert_eq!(
            *totals.lock().unwrap(),
            Totals {
                counter: 10,
                counter_series: if off { 2 } else { 1 },
                histogram_sum: 10,
                histogram_count: 2,
                histogram_series: if off { 2 } else { 1 },
                buckets: vec![1, 1],
                unexpected_data: false,
            }
        );
        provider.inner.shutdown().unwrap();
    }
}
impl PushMetricExporter for Capture {
    async fn export(&self, metrics: &ResourceMetrics) -> OTelSdkResult {
        self.0.lock().unwrap().push(format!("{metrics:?}"));
        Ok(())
    }
    fn force_flush(&self) -> OTelSdkResult {
        Ok(())
    }
    fn shutdown_with_timeout(&self, _: Duration) -> OTelSdkResult {
        Ok(())
    }
    fn temporality(&self) -> Temporality {
        Temporality::Cumulative
    }
}
#[test]
fn every_instrument_and_callback_exports_filtered_labels() {
    for mode in [Mode::Public, Mode::Strict, Mode::Off] {
        let off = matches!(mode, Mode::Off);
        let strict = matches!(mode, Mode::Strict);
        let captured = Arc::new(Mutex::new(Vec::new()));
        let inner = SdkMeterProvider::builder()
            .with_reader(
                PeriodicReader::builder(Capture(captured.clone()))
                    .with_interval(Duration::from_secs(3600))
                    .build(),
            )
            .build();
        let provider = ProtectedMeterProvider {
            inner,
            policy: Policy::new(mode, "account_number").unwrap(),
            state: Default::default(),
        };
        let meter = provider.meter("fixture");
        let labels = vec![
            KeyValue::new("email", "builtin-secret"),
            KeyValue::new("account_number", "custom-secret"),
        ];
        macro_rules! record {
            ($method:ident, $operation:ident, $value:expr) => {
                meter
                    .$method(stringify!($method))
                    .with_description("metadata-description-secret")
                    .with_unit("fixture-unit")
                    .build()
                    .$operation($value, &labels);
            };
        }
        record!(u64_counter, add, 1);
        record!(f64_counter, add, 1.0);
        record!(i64_up_down_counter, add, -1);
        record!(f64_up_down_counter, add, -1.0);
        record!(u64_gauge, record, 2);
        record!(i64_gauge, record, -2);
        record!(f64_gauge, record, 2.0);
        record!(u64_histogram, record, 3);
        record!(f64_histogram, record, 3.0);
        macro_rules! observe {
            ($method:ident, $value:expr) => {{
                let labels = labels.clone();
                meter
                    .$method(stringify!($method))
                    .with_description("metadata-description-secret")
                    .with_unit("fixture-unit")
                    .with_callback(move |instrument| instrument.observe($value, &labels))
                    .build()
            }};
        }
        let _c1 = observe!(u64_observable_counter, 1);
        let _c2 = observe!(f64_observable_counter, 1.0);
        let _c3 = observe!(i64_observable_up_down_counter, -1);
        let _c4 = observe!(f64_observable_up_down_counter, -1.0);
        let _c5 = observe!(u64_observable_gauge, 2);
        let _c6 = observe!(i64_observable_gauge, -2);
        let _c7 = observe!(f64_observable_gauge, 2.0);
        provider.inner.force_flush().unwrap();
        provider.inner.shutdown().unwrap();
        let payload = captured.lock().unwrap().join("\n");
        for method in [
            "u64_counter",
            "f64_counter",
            "i64_up_down_counter",
            "f64_up_down_counter",
            "u64_gauge",
            "i64_gauge",
            "f64_gauge",
            "u64_histogram",
            "f64_histogram",
            "u64_observable_counter",
            "f64_observable_counter",
            "i64_observable_up_down_counter",
            "f64_observable_up_down_counter",
            "u64_observable_gauge",
            "i64_observable_gauge",
            "f64_observable_gauge",
        ] {
            assert!(
                payload.contains(&format!("name: \"{method}\"")),
                "missing {method}"
            );
        }
        assert_eq!(payload.contains("builtin-secret"), off);
        assert_eq!(payload.contains("custom-secret"), off);
        assert_eq!(payload.contains("[REDACTED]"), !off);
        assert_eq!(payload.contains("metadata-description-secret"), off);
        assert_eq!(payload.contains("fixture-unit"), !strict);
    }
}
