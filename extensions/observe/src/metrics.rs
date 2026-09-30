//! Policy-enforcing metric instruments. No raw SDK provider is exposed.
use crate::redaction::Policy;
use opentelemetry::{InstrumentationScope, KeyValue, metrics::*};
use opentelemetry_sdk::metrics::SdkMeterProvider;
use std::sync::Arc;

pub struct ProtectedMeterProvider {
    pub(crate) inner: SdkMeterProvider,
    pub(crate) policy: Policy,
}
impl MeterProvider for ProtectedMeterProvider {
    fn meter_with_scope(&self, scope: InstrumentationScope) -> Meter {
        Meter::new(Arc::new(ProtectedInstruments {
            inner: self.inner.meter_with_scope(scope),
            policy: self.policy.clone(),
        }))
    }
}
struct ProtectedInstruments {
    inner: Meter,
    policy: Policy,
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
    asynchronous!(u64_observable_counter, ObservableCounter, u64);
    asynchronous!(f64_observable_counter, ObservableCounter, f64);
    asynchronous!(i64_observable_up_down_counter, ObservableUpDownCounter, i64);
    asynchronous!(f64_observable_up_down_counter, ObservableUpDownCounter, f64);
    asynchronous!(u64_observable_gauge, ObservableGauge, u64);
    asynchronous!(i64_observable_gauge, ObservableGauge, i64);
    asynchronous!(f64_observable_gauge, ObservableGauge, f64);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redaction::Mode;
    use opentelemetry_sdk::{
        error::OTelSdkResult,
        metrics::{
            PeriodicReader, Temporality, data::ResourceMetrics, exporter::PushMetricExporter,
        },
    };
    use std::{sync::Mutex, time::Duration};
    struct Capture(Arc<Mutex<Vec<String>>>);
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
