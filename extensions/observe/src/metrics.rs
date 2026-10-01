//! Policy-enforcing metric instruments. No raw SDK provider is exposed.
use crate::redaction::Policy;
use opentelemetry::{InstrumentationScope, KeyValue, metrics::*};
use opentelemetry_sdk::metrics::SdkMeterProvider;
use std::sync::Arc;

/// Per-observable-instrument collection bounds; exceeding them fails the provider.
pub const MAX_OBSERVABLE_GROUPS: usize = 1024;
pub const MAX_OBSERVABLE_ATTRIBUTES: usize = 64;
pub const MAX_OBSERVABLE_ATTRIBUTE_BYTES: usize = 8192;
/// Provider-lifetime distinct filtered scope and instrument-definition budgets.
pub const MAX_METERS: usize = 128;
pub const MAX_INSTRUMENTS: usize = 1024;
pub const MAX_HISTOGRAM_BOUNDARIES: usize = 128;
/// SDK per-stream cardinality budget; overflow uses a separate aggregation point.
pub const MAX_STREAM_SERIES: usize = 1024;

/// Shared production/test SDK configuration; no aggregation override, so caller
/// histogram boundaries and SDK instrument-specific aggregation stay intact.
pub(crate) fn bounded_meter_builder()
-> Result<opentelemetry_sdk::metrics::MeterProviderBuilder, &'static str> {
    opentelemetry_sdk::metrics::Stream::builder()
        .with_cardinality_limit(MAX_STREAM_SERIES)
        .build()
        .map_err(|_| "invalid metric cardinality budget")?;
    Ok(SdkMeterProvider::builder().with_view(|_| {
        // Only the validated constant is used; matching cannot introduce a new
        // stream validation failure from caller metadata.
        opentelemetry_sdk::metrics::Stream::builder()
            .with_cardinality_limit(MAX_STREAM_SERIES)
            .build()
            .ok()
    }))
}
#[derive(Hash, Eq, PartialEq)]
struct InstrumentIdentity {
    scope: InstrumentationScope,
    kind: &'static str,
    name: String,
    description: Option<String>,
    unit: Option<String>,
    boundaries: Option<Vec<u64>>,
}
struct InstrumentInput<'a> {
    kind: &'static str,
    name: &'a str,
    description: Option<&'a str>,
    unit: Option<&'a str>,
    boundaries: Option<&'a [f64]>,
}

fn valid_instrument_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    !name.is_empty()
        && name.len() <= 255
        && bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic())
        && bytes
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-' | b'/'))
}
struct RejectedInstrument;
impl<T> SyncInstrument<T> for RejectedInstrument {
    fn measure(&self, _: T, _: &[KeyValue]) {}
}

fn labels_within_budget(attributes: &[KeyValue]) -> bool {
    attribute_iterator_within_budget(attributes.iter())
}
fn attribute_iterator_within_budget<'a>(attributes: impl Iterator<Item = &'a KeyValue>) -> bool {
    use opentelemetry::{Array, Value};
    let mut bytes = 0usize;
    for (index, attribute) in attributes.enumerate() {
        if index >= MAX_OBSERVABLE_ATTRIBUTES {
            return false;
        }
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
pub(crate) struct MetricState(
    Arc<std::sync::atomic::AtomicBool>,
    Arc<std::sync::Mutex<std::collections::HashSet<(InstrumentationScope, String)>>>,
    Arc<std::sync::Mutex<std::collections::HashSet<InstrumentationScope>>>,
    Arc<std::sync::Mutex<std::collections::HashSet<InstrumentIdentity>>>,
);
impl MetricState {
    fn register_meter(&self, scope: &InstrumentationScope) -> bool {
        if self.failed() {
            return false;
        }
        let Ok(mut scopes) = self.2.lock() else {
            self.fail();
            return false;
        };
        if self.failed() {
            return false;
        }
        if scopes.contains(scope) {
            return true;
        }
        if scopes.len() >= MAX_METERS {
            self.fail();
            return false;
        }
        scopes.insert(scope.clone());
        true
    }
    fn register_instrument(
        &self,
        scope: &InstrumentationScope,
        policy: &Policy,
        input: InstrumentInput<'_>,
    ) -> bool {
        if self.failed() {
            return false;
        }
        if !valid_instrument_name(input.name)
            || input.description.is_some_and(|text| text.len() > 1024)
            || input
                .unit
                .is_some_and(|text| text.len() > 64 || text.chars().any(char::is_control))
            || input
                .boundaries
                .is_some_and(|bounds| bounds.len() > MAX_HISTOGRAM_BOUNDARIES)
        {
            self.fail();
            return false;
        }
        let identity = InstrumentIdentity {
            scope: scope.clone(),
            kind: input.kind,
            name: input.name.to_owned(),
            description: policy
                .metric_description(input.description.map(|text| text.to_owned().into()))
                .map(|text| text.into_owned()),
            unit: policy
                .metric_unit(input.unit.map(|text| text.to_owned().into()))
                .map(|text| text.into_owned()),
            boundaries: input
                .boundaries
                .map(|bounds| bounds.iter().map(|value| value.to_bits()).collect()),
        };
        let Ok(mut instruments) = self.3.lock() else {
            self.fail();
            return false;
        };
        if self.failed() {
            return false;
        }
        if instruments.contains(&identity) {
            return true;
        }
        if instruments.len() >= MAX_INSTRUMENTS {
            self.fail();
            return false;
        }
        instruments.insert(identity);
        true
    }
    fn fail(&self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }
    pub(crate) fn failed(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }
    fn register_observable(&self, scope: &InstrumentationScope, name: &str) -> bool {
        if self.failed() {
            return false;
        }
        if !valid_instrument_name(name) {
            self.fail();
            return false;
        }
        let Ok(mut registered) = self.1.lock() else {
            self.fail();
            return false;
        };
        if registered.len() >= MAX_OBSERVABLE_GROUPS
            || !registered.insert((scope.clone(), name.to_owned()))
        {
            self.fail();
            return false;
        }
        true
    }
}

pub struct ProtectedMeterProvider {
    pub(crate) inner: SdkMeterProvider,
    pub(crate) policy: Policy,
    pub(crate) state: MetricState,
}
impl MeterProvider for ProtectedMeterProvider {
    fn meter_with_scope(&self, scope: InstrumentationScope) -> Meter {
        let valid_text = |value: &str, limit: usize| {
            value.len() <= limit && !value.chars().any(char::is_control)
        };
        if scope.name().is_empty()
            || !valid_text(scope.name(), 255)
            || scope.version().is_some_and(|value| !valid_text(value, 255))
            || scope
                .schema_url()
                .is_some_and(|value| !valid_text(value, 2048))
            || !attribute_iterator_within_budget(scope.attributes())
        {
            self.state.fail();
            return NoopMeterProvider::new().meter("rejected-scope");
        }
        let scope = self.policy.scope(&scope);
        if !self.state.register_meter(&scope) {
            return NoopMeterProvider::new().meter("rejected-scope");
        }
        Meter::new(Arc::new(ProtectedInstruments {
            inner: self.inner.meter_with_scope(scope.clone()),
            scope,
            policy: self.policy.clone(),
            state: self.state.clone(),
        }))
    }
}
struct ProtectedInstruments {
    scope: InstrumentationScope,
    inner: Meter,
    policy: Policy,
    state: MetricState,
}
struct Recording<T> {
    record: Box<dyn Fn(T, &[KeyValue]) + Send + Sync>,
    policy: Policy,
    state: MetricState,
}
impl<T> SyncInstrument<T> for Recording<T> {
    fn measure(&self, value: T, attributes: &[KeyValue]) {
        if self.state.failed() {
            return;
        }
        if !labels_within_budget(attributes) {
            self.state.fail();
            return;
        }
        (self.record)(value, &self.policy.metric_attributes(attributes.to_vec()));
    }
}
struct Observing<'a, T> {
    inner: &'a dyn AsyncInstrument<T>,
    policy: &'a Policy,
    state: &'a MetricState,
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
    originals: std::sync::Mutex<Vec<Vec<KeyValue>>>,
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
        let mut original = attributes.to_vec();
        original.sort_by(|a, b| a.key.as_str().cmp(b.key.as_str()));
        if original.windows(2).any(|pair| pair[0].key == pair[1].key) {
            self.state.fail();
            return;
        }
        // Bounded collection-local originals distinguish duplicates from masked collisions.
        let Ok(mut originals) = self.originals.lock() else {
            self.state.fail();
            return;
        };
        if self.state.failed() {
            return;
        }
        if originals.contains(&original) || originals.len() >= MAX_OBSERVABLE_GROUPS {
            self.state.fail();
            return;
        }
        let attributes = self.policy.metric_attributes(original.clone());
        originals.push(original);
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
        if self.state.failed() {
            return;
        }
        if !labels_within_budget(attributes) {
            self.state.fail();
            return;
        }
        self.inner
            .observe(value, &self.policy.metric_attributes(attributes.to_vec()));
    }
}
macro_rules! sync_instrument {
    ($method:ident, $kind:ident, $number:ty, $operation:ident) => {
        fn $method(&self, builder: InstrumentBuilder<'_, $kind<$number>>) -> $kind<$number> {
            if !self.state.register_instrument(
                &self.scope,
                &self.policy,
                InstrumentInput {
                    kind: stringify!($method),
                    name: &builder.name,
                    description: builder.description.as_deref(),
                    unit: builder.unit.as_deref(),
                    boundaries: None,
                },
            ) {
                self.state.fail();
                return $kind::new(Arc::new(RejectedInstrument));
            }
            let mut delegated = self.inner.$method(builder.name);
            if let Some(value) = self.policy.metric_description(builder.description) {
                delegated = delegated.with_description(value);
            }
            if let Some(value) = self.policy.metric_unit(builder.unit) {
                delegated = delegated.with_unit(value);
            }
            let instrument = delegated.build();
            $kind::new(Arc::new(Recording {
                record: Box::new(move |value, attributes| instrument.$operation(value, attributes)),
                policy: self.policy.clone(),
                state: self.state.clone(),
            }))
        }
    };
}
macro_rules! histogram {
    ($method:ident, $number:ty) => {
        fn $method(&self, builder: HistogramBuilder<'_, Histogram<$number>>) -> Histogram<$number> {
            if !self.state.register_instrument(
                &self.scope,
                &self.policy,
                InstrumentInput {
                    kind: stringify!($method),
                    name: &builder.name,
                    description: builder.description.as_deref(),
                    unit: builder.unit.as_deref(),
                    boundaries: builder.boundaries.as_deref(),
                },
            ) {
                self.state.fail();
                return Histogram::new(Arc::new(RejectedInstrument));
            }
            let mut delegated = self.inner.$method(builder.name);
            if let Some(value) = self.policy.metric_description(builder.description) {
                delegated = delegated.with_description(value);
            }
            if let Some(value) = self.policy.metric_unit(builder.unit) {
                delegated = delegated.with_unit(value);
            }
            if let Some(value) = builder.boundaries {
                delegated = delegated.with_boundaries(value);
            }
            let instrument = delegated.build();
            Histogram::new(Arc::new(Recording {
                record: Box::new(move |value, attributes| instrument.record(value, attributes)),
                policy: self.policy.clone(),
                state: self.state.clone(),
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
            if !self.state.register_instrument(
                &self.scope,
                &self.policy,
                InstrumentInput {
                    kind: stringify!($method),
                    name: &builder.name,
                    description: builder.description.as_deref(),
                    unit: builder.unit.as_deref(),
                    boundaries: None,
                },
            ) || !self.state.register_observable(&self.scope, &builder.name)
            {
                return $kind::new();
            }
            let mut delegated = self.inner.$method(builder.name);
            if let Some(value) = self.policy.metric_description(builder.description) {
                delegated = delegated.with_description(value);
            }
            if let Some(value) = self.policy.metric_unit(builder.unit) {
                delegated = delegated.with_unit(value);
            }
            for callback in builder.callbacks {
                let policy = self.policy.clone();
                let state = self.state.clone();
                delegated = delegated.with_callback(move |inner| {
                    if state.failed() {
                        return;
                    }
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        callback(&Observing {
                            inner,
                            policy: &policy,
                            state: &state,
                        })
                    }))
                    .is_err()
                    {
                        state.fail();
                    }
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
            if !self.state.register_instrument(
                &self.scope,
                &self.policy,
                InstrumentInput {
                    kind: stringify!($method),
                    name: &builder.name,
                    description: builder.description.as_deref(),
                    unit: builder.unit.as_deref(),
                    boundaries: None,
                },
            ) || !self.state.register_observable(&self.scope, &builder.name)
            {
                return $kind::new();
            }
            let mut delegated = self.inner.$method(builder.name);
            if let Some(value) = self.policy.metric_description(builder.description) {
                delegated = delegated.with_description(value);
            }
            if let Some(value) = self.policy.metric_unit(builder.unit) {
                delegated = delegated.with_unit(value);
            }
            let policy = self.policy.clone();
            let callbacks = builder.callbacks;
            let state = self.state.clone();
            delegated
                .with_callback(move |inner| {
                    if state.failed() {
                        return;
                    }
                    let observations = Summing {
                        groups: std::sync::Mutex::new(Vec::new()),
                        originals: std::sync::Mutex::new(Vec::new()),
                        policy: &policy,
                        state: &state,
                    };
                    for callback in &callbacks {
                        if state.failed() {
                            break;
                        }
                        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            callback(&observations)
                        }))
                        .is_err()
                        {
                            state.fail();
                            break;
                        }
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
mod tests;
