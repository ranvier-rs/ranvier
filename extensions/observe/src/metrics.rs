//! Policy-enforcing metric instruments. No raw SDK provider is exposed.
use crate::redaction::Policy;
use opentelemetry::{InstrumentationScope, KeyValue, metrics::*};
use opentelemetry_sdk::metrics::SdkMeterProvider;
use std::sync::Arc;

/// Per-observable-instrument collection bounds; exceeding them fails the provider.
pub const MAX_OBSERVABLE_GROUPS: usize = 1024;
pub const MAX_OBSERVABLE_ATTRIBUTES: usize = 64;
pub const MAX_OBSERVABLE_ATTRIBUTE_BYTES: usize = 8192;

fn labels_within_budget(attributes: &[KeyValue]) -> bool {
    use opentelemetry::{Array, Value};
    if attributes.len() > MAX_OBSERVABLE_ATTRIBUTES {
        return false;
    }
    let mut bytes = 0usize;
    for attribute in attributes {
        let size = match &attribute.value {
            Value::String(value) => Some(value.as_str().len()),
            Value::Bool(_) => Some(1),
            Value::I64(_) | Value::F64(_) => Some(8),
            Value::Array(Array::Bool(values)) => Some(values.len()),
            Value::Array(Array::I64(values)) => values.len().checked_mul(8),
            Value::Array(Array::F64(values)) => values.len().checked_mul(8),
            Value::Array(Array::String(values)) => values.iter().try_fold(0usize, |sum, value| {
                sum.checked_add(std::mem::size_of::<opentelemetry::StringValue>())
                    .and_then(|size| size.checked_add(value.as_str().len()))
                    .filter(|size| *size <= MAX_OBSERVABLE_ATTRIBUTE_BYTES)
            }),
            _ => return false,
        };
        let Some(total) = size
            .and_then(|size| size.checked_add(attribute.key.as_str().len()))
            .and_then(|size| bytes.checked_add(size))
        else {
            return false;
        };
        bytes = total;
        if bytes > MAX_OBSERVABLE_ATTRIBUTE_BYTES {
            return false;
        }
    }
    true
}

#[derive(Clone, Default)]
pub(crate) struct MetricState(Arc<std::sync::atomic::AtomicBool>);
impl MetricState {
    fn fail(&self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }
    pub(crate) fn failed(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }
}

pub struct ProtectedMeterProvider {
    pub(crate) inner: SdkMeterProvider,
    pub(crate) policy: Policy,
    pub(crate) state: MetricState,
}
impl MeterProvider for ProtectedMeterProvider {
    fn meter_with_scope(&self, scope: InstrumentationScope) -> Meter {
        Meter::new(Arc::new(ProtectedInstruments {
            inner: self.inner.meter_with_scope(scope),
            policy: self.policy.clone(),
            state: self.state.clone(),
        }))
    }
}
struct ProtectedInstruments {
    inner: Meter,
    policy: Policy,
    state: MetricState,
}
struct Recording<T> {
    record: Box<dyn Fn(T, &[KeyValue]) + Send + Sync>,
    policy: Policy,
}
impl<T> SyncInstrument<T> for Recording<T> {
    fn measure(&self, value: T, attributes: &[KeyValue]) {
        (self.record)(value, &self.policy.metric_attributes(attributes.to_vec()));
    }
}
struct Observing<'a, T> {
    inner: &'a dyn AsyncInstrument<T>,
    policy: &'a Policy,
}
trait CheckedSum: Copy + Send {
    fn checked_sum(self, other: Self) -> Option<Self>;
    fn valid(self) -> bool {
        true
    }
}
impl CheckedSum for u64 {
    fn checked_sum(self, other: Self) -> Option<Self> {
        self.checked_add(other)
    }
}
impl CheckedSum for i64 {
    fn checked_sum(self, other: Self) -> Option<Self> {
        self.checked_add(other)
    }
}
impl CheckedSum for f64 {
    fn valid(self) -> bool {
        self.is_finite()
    }
    fn checked_sum(self, other: Self) -> Option<Self> {
        let sum = self + other;
        sum.is_finite().then_some(sum)
    }
}
struct Summing<'a, T> {
    groups: std::sync::Mutex<Vec<(Vec<KeyValue>, T)>>,
    policy: &'a Policy,
    state: &'a MetricState,
}
impl<T: CheckedSum> AsyncInstrument<T> for Summing<'_, T> {
    fn observe(&self, value: T, attributes: &[KeyValue]) {
        if self.state.failed() {
            return;
        }
        if !value.valid() || !labels_within_budget(attributes) {
            self.state.fail();
            return;
        }
        let mut attributes = self.policy.metric_attributes(attributes.to_vec());
        attributes.sort_by(|a, b| a.key.as_str().cmp(b.key.as_str()));
        if attributes.windows(2).any(|pair| pair[0].key == pair[1].key) {
            self.state.fail();
            return;
        }
        let Ok(mut groups) = self.groups.lock() else {
            self.state.fail();
            return;
        };
        // Recheck after waiting for a concurrent observer that may have failed.
        if self.state.failed() {
            return;
        }
        if let Some((_, total)) = groups.iter_mut().find(|(labels, _)| *labels == attributes) {
            if let Some(sum) = total.checked_sum(value) {
                *total = sum;
            } else {
                self.state.fail();
            }
        } else {
            if groups.len() >= MAX_OBSERVABLE_GROUPS {
                self.state.fail();
                return;
            }
            groups.push((attributes, value));
        }
    }
}
impl<T> AsyncInstrument<T> for Observing<'_, T> {
    fn observe(&self, value: T, attributes: &[KeyValue]) {
        self.inner
            .observe(value, &self.policy.metric_attributes(attributes.to_vec()));
    }
}
macro_rules! sync_instrument {
    ($method:ident, $kind:ident, $number:ty, $operation:ident) => {
        fn $method(&self, builder: InstrumentBuilder<'_, $kind<$number>>) -> $kind<$number> {
            let mut delegated = self.inner.$method(builder.name);
            if let Some(value) = builder.description {
                delegated = delegated.with_description(value);
            }
            if let Some(value) = builder.unit {
                delegated = delegated.with_unit(value);
            }
            let instrument = delegated.build();
            $kind::new(Arc::new(Recording {
                record: Box::new(move |value, attributes| instrument.$operation(value, attributes)),
                policy: self.policy.clone(),
            }))
        }
    };
}
macro_rules! histogram {
    ($method:ident, $number:ty) => {
        fn $method(&self, builder: HistogramBuilder<'_, Histogram<$number>>) -> Histogram<$number> {
            let mut delegated = self.inner.$method(builder.name);
            if let Some(value) = builder.description {
                delegated = delegated.with_description(value);
            }
            if let Some(value) = builder.unit {
                delegated = delegated.with_unit(value);
            }
            if let Some(value) = builder.boundaries {
                delegated = delegated.with_boundaries(value);
            }
            let instrument = delegated.build();
            Histogram::new(Arc::new(Recording {
                record: Box::new(move |value, attributes| instrument.record(value, attributes)),
                policy: self.policy.clone(),
            }))
        }
    };
}
macro_rules! asynchronous {
    ($method:ident, $kind:ident, $number:ty) => {
        fn $method(
            &self,
            builder: AsyncInstrumentBuilder<'_, $kind<$number>, $number>,
        ) -> $kind<$number> {
            let mut delegated = self.inner.$method(builder.name);
            if let Some(value) = builder.description {
                delegated = delegated.with_description(value);
            }
            if let Some(value) = builder.unit {
                delegated = delegated.with_unit(value);
            }
            for callback in builder.callbacks {
                let policy = self.policy.clone();
                delegated = delegated.with_callback(move |inner| {
                    callback(&Observing {
                        inner,
                        policy: &policy,
                    })
                });
            }
            delegated.build()
        }
    };
}
macro_rules! asynchronous_sum {
    ($method:ident, $kind:ident, $number:ty) => {
        fn $method(
            &self,
            builder: AsyncInstrumentBuilder<'_, $kind<$number>, $number>,
        ) -> $kind<$number> {
            let mut delegated = self.inner.$method(builder.name);
            if let Some(value) = builder.description {
                delegated = delegated.with_description(value);
            }
            if let Some(value) = builder.unit {
                delegated = delegated.with_unit(value);
            }
            let policy = self.policy.clone();
            let callbacks = builder.callbacks;
            let state = self.state.clone();
            delegated
                .with_callback(move |inner| {
                    let observations = Summing {
                        groups: std::sync::Mutex::new(Vec::new()),
                        policy: &policy,
                        state: &state,
                    };
                    for callback in &callbacks {
                        callback(&observations);
                    }
                    if state.failed() {
                        return;
                    }
                    match observations.groups.into_inner() {
                        Ok(groups) => {
                            for (attributes, value) in groups {
                                inner.observe(value, &attributes);
                            }
                        }
                        Err(_) => state.fail(),
                    }
                })
                .build()
        }
    };
}
impl InstrumentProvider for ProtectedInstruments {
    sync_instrument!(u64_counter, Counter, u64, add);
    sync_instrument!(f64_counter, Counter, f64, add);
    sync_instrument!(i64_up_down_counter, UpDownCounter, i64, add);
    sync_instrument!(f64_up_down_counter, UpDownCounter, f64, add);
    sync_instrument!(u64_gauge, Gauge, u64, record);
    sync_instrument!(i64_gauge, Gauge, i64, record);
    sync_instrument!(f64_gauge, Gauge, f64, record);
    histogram!(u64_histogram, u64);
    histogram!(f64_histogram, f64);
    asynchronous_sum!(u64_observable_counter, ObservableCounter, u64);
    asynchronous_sum!(f64_observable_counter, ObservableCounter, f64);
    asynchronous_sum!(i64_observable_up_down_counter, ObservableUpDownCounter, i64);
    asynchronous_sum!(f64_observable_up_down_counter, ObservableUpDownCounter, f64);
    asynchronous!(u64_observable_gauge, ObservableGauge, u64);
    asynchronous!(i64_observable_gauge, ObservableGauge, i64);
    asynchronous!(f64_observable_gauge, ObservableGauge, f64);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn concurrent_observers_normalize_key_order_without_losing_sums() {
        let policy = Policy::new(crate::redaction::Mode::Public, "").unwrap();
        let state = MetricState::default();
        let observations = Summing {
            groups: std::sync::Mutex::new(Vec::new()),
            policy: &policy,
            state: &state,
        };
        std::thread::scope(|scope| {
            for index in 0..8 {
                let observations = &observations;
                scope.spawn(move || {
                    for _ in 0..10 {
                        let mut labels = vec![
                            KeyValue::new("email", format!("synthetic-{index}")),
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
            policy: &policy,
            state: &state,
        };
        for index in 0..MAX_OBSERVABLE_GROUPS {
            observations.observe(1_u64, &[KeyValue::new("index", index as i64)]);
        }
        assert!(!state.failed());
        observations.observe(1, &[KeyValue::new("index", 0_i64)]);
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
            policy: &policy,
            state: &state,
        };
        observations.observe(u64::MAX, &[]);
        observations.observe(1_u64, &[]);
        assert!(state.failed());
        let policy = Policy::new(crate::redaction::Mode::Public, "").unwrap();
        let state = MetricState::default();
        let observations = Summing {
            groups: std::sync::Mutex::new(Vec::new()),
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
        metrics::{
            PeriodicReader, Temporality, data::ResourceMetrics, exporter::PushMetricExporter,
        },
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
    #[test]
    fn masking_merges_series_without_losing_counter_or_histogram_totals() {
        for mode in [Mode::Public, Mode::Strict, Mode::Off] {
            let off = matches!(mode, Mode::Off);
            let totals = Arc::new(Mutex::new(Totals::default()));
            let provider = ProtectedMeterProvider {
                inner: SdkMeterProvider::builder()
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
        }
    }
}
