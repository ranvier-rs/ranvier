//! # ranvier-test — Test Utilities for Ranvier Pipelines
//!
//! Provides lightweight helpers that reduce boilerplate when unit-testing
//! Transitions and Axon pipelines.
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use ranvier_test::{TestBus, TestAxon, assert_outcome_ok, assert_outcome_err};
//! use ranvier_runtime::Axon;
//!
//! #[tokio::test]
//! async fn my_pipeline_test() {
//!     let bus = TestBus::new().with(42_i32).with("config".to_string());
//!     let axon = Axon::simple::<String>("test").then(my_transition);
//!     let outcome = TestAxon::run(axon, (), &(), bus).await;
//!     assert_outcome_ok!(outcome, |val| assert_eq!(val, expected));
//! }
//! ```

pub use ranvier_core::prelude::*;

/// A builder for pre-populated test Bus instances.
///
/// Provides a fluent API for inserting typed values before pipeline execution.
///
/// # Example
///
/// ```rust,ignore
/// let bus = TestBus::new()
///     .with(42_i32)
///     .with("hello".to_string())
///     .build();
/// assert_eq!(*bus.read::<i32>().unwrap(), 42);
/// ```
pub struct TestBus {
    bus: Bus,
}

impl TestBus {
    /// Create a new empty test Bus.
    pub fn new() -> Self {
        Self { bus: Bus::new() }
    }

    /// Insert a typed value into the Bus (builder pattern).
    pub fn with<T: Send + Sync + 'static>(mut self, value: T) -> Self {
        self.bus.insert(value);
        self
    }

    /// Consume the builder and return the underlying Bus.
    pub fn build(self) -> Bus {
        self.bus
    }
}

impl Default for TestBus {
    fn default() -> Self {
        Self::new()
    }
}

/// Convenience wrapper for executing an Axon in tests.
///
/// Handles Bus construction and provides a single-call entry point.
pub struct TestAxon;

impl TestAxon {
    /// Execute an Axon with the given input, resources, and pre-built Bus.
    ///
    /// Returns the `Outcome` and the Bus (for post-execution inspection).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let bus = TestBus::new().with(42_i32);
    /// let (outcome, bus) = TestAxon::run(axon, (), &(), bus).await;
    /// assert_outcome_ok!(outcome);
    /// assert!(bus.read::<TransitionErrorContext>().is_none());
    /// ```
    pub async fn run<In, Out, E, Res>(
        axon: ranvier_runtime::Axon<In, Out, E, Res>,
        input: In,
        resources: &Res,
        test_bus: TestBus,
    ) -> (Outcome<Out, E>, Bus)
    where
        In: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        Out: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
        Res: ranvier_core::transition::ResourceRequirement,
    {
        let mut bus = test_bus.build();
        let outcome = axon.execute(input, resources, &mut bus).await;
        (outcome, bus)
    }
}

/// Assert that an `Outcome` is `Next` (success).
///
/// Optionally accepts a closure to inspect the inner value.
///
/// # Examples
///
/// ```rust,ignore
/// assert_outcome_ok!(outcome);
/// assert_outcome_ok!(outcome, |val| assert_eq!(val, 42));
/// ```
#[macro_export]
macro_rules! assert_outcome_ok {
    ($outcome:expr) => {
        match &$outcome {
            $crate::Outcome::Next(_) => {}
            other => panic!(
                "expected Outcome::Next, got {:?}",
                $crate::__outcome_variant_name(other),
            ),
        }
    };
    ($outcome:expr, $check:expr) => {
        match $outcome {
            $crate::Outcome::Next(val) => {
                let check: fn(_) = $check;
                check(val);
            }
            other => panic!(
                "expected Outcome::Next, got {:?}",
                $crate::__outcome_variant_name(&other),
            ),
        }
    };
}

/// Assert that an `Outcome` is `Fault` (error).
///
/// Optionally accepts a closure to inspect the error value.
///
/// # Examples
///
/// ```rust,ignore
/// assert_outcome_err!(outcome);
/// assert_outcome_err!(outcome, |err| assert_eq!(err, "boom"));
/// ```
#[macro_export]
macro_rules! assert_outcome_err {
    ($outcome:expr) => {
        match &$outcome {
            $crate::Outcome::Fault(_) => {}
            other => panic!(
                "expected Outcome::Fault, got {:?}",
                $crate::__outcome_variant_name(other),
            ),
        }
    };
    ($outcome:expr, $check:expr) => {
        match $outcome {
            $crate::Outcome::Fault(err) => {
                let check: fn(_) = $check;
                check(err);
            }
            other => panic!(
                "expected Outcome::Fault, got {:?}",
                $crate::__outcome_variant_name(&other),
            ),
        }
    };
}

/// Assert that a collected stream has the expected number of items.
///
/// Optionally accepts a closure to inspect the first item.
///
/// # Examples
///
/// ```rust,ignore
/// let items = TestAxon::run_stream(streaming_axon, input, &(), TestBus::new()).await.unwrap();
/// assert_stream_items!(items, 3);
/// assert_stream_items!(items, 3, |first| assert_eq!(first.text, "Hello"));
/// ```
#[macro_export]
macro_rules! assert_stream_items {
    ($items:expr, $count:expr) => {
        assert_eq!(
            $items.len(),
            $count,
            "expected {} stream items, got {}",
            $count,
            $items.len()
        );
    };
    ($items:expr, $count:expr, $check_first:expr) => {
        assert_eq!(
            $items.len(),
            $count,
            "expected {} stream items, got {}",
            $count,
            $items.len()
        );
        if !$items.is_empty() {
            let check: fn(&_) = $check_first;
            check(&$items[0]);
        }
    };
}

#[cfg(feature = "streaming")]
impl TestAxon {
    /// Execute a `StreamingAxon` and collect all items into a `Vec`.
    ///
    /// Returns the collected items or a `StreamingAxonError`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let items = TestAxon::run_stream(streaming_axon, input, &(), TestBus::new()).await.unwrap();
    /// assert_stream_items!(items, 5);
    /// ```
    pub async fn run_stream<In, Item, E, Res>(
        axon: ranvier_runtime::StreamingAxon<In, Item, E, Res>,
        input: In,
        resources: &Res,
        test_bus: TestBus,
    ) -> Result<Vec<Item>, ranvier_runtime::StreamingAxonError<E>>
    where
        In: Send + Sync + 'static,
        Item: Send + 'static,
        E: Send + Sync + std::fmt::Debug + 'static,
        Res: ranvier_core::transition::ResourceRequirement,
    {
        use futures_util::StreamExt;
        let mut bus = test_bus.build();
        let stream = axon.execute(input, resources, &mut bus).await?;
        Ok(stream.collect().await)
    }
}

/// Internal helper — returns a variant name for panic messages.
#[doc(hidden)]
pub fn __outcome_variant_name<T, E: std::fmt::Debug>(outcome: &Outcome<T, E>) -> &'static str {
    match outcome {
        Outcome::Next(_) => "Next",
        Outcome::Fault(_) => "Fault",
        Outcome::Branch(_, _) => "Branch",
        Outcome::Emit { .. } => "Emit",
        Outcome::Jump(_, _) => "Jump",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bus_builder() {
        let bus = TestBus::new()
            .with(42_i32)
            .with("hello".to_string())
            .build();
        assert_eq!(*bus.read::<i32>().unwrap(), 42);
        assert_eq!(bus.read::<String>().unwrap(), "hello");
    }

    #[test]
    fn test_bus_default_is_empty() {
        let bus = TestBus::default().build();
        assert!(bus.read::<i32>().is_none());
    }

    #[tokio::test]
    async fn test_axon_run_success() {
        let axon = ranvier_runtime::Axon::simple::<String>("test")
            .then_fn("add-greeting", |_: (), _bus: &mut Bus| {
                Outcome::Next("hello".to_string())
            });
        let (outcome, _bus) = TestAxon::run(axon, (), &(), TestBus::new()).await;
        assert_outcome_ok!(outcome, |val: String| assert_eq!(val, "hello"));
    }

    #[tokio::test]
    async fn test_axon_run_fault() {
        let axon = ranvier_runtime::Axon::simple::<String>("test")
            .then_fn("fail", |_: (), _bus: &mut Bus| {
                Outcome::<String, String>::Fault("boom".to_string())
            });
        let (outcome, bus) = TestAxon::run(axon, (), &(), TestBus::new()).await;
        assert_outcome_err!(outcome, |err: String| assert_eq!(err, "boom"));
        // TransitionErrorContext should be in Bus
        let ctx = bus.read::<ranvier_core::error::TransitionErrorContext>().unwrap();
        assert_eq!(ctx.transition_name, "fail");
    }

    #[tokio::test]
    async fn test_bus_with_pre_populated_values() {
        let axon = ranvier_runtime::Axon::simple::<String>("test")
            .then_fn("read-bus", |_: (), bus: &mut Bus| {
                let val = bus.read::<i32>().copied().unwrap_or(0);
                Outcome::Next(val)
            });
        let (outcome, _bus) = TestAxon::run(
            axon, (), &(),
            TestBus::new().with(99_i32),
        ).await;
        assert_outcome_ok!(outcome, |val: i32| assert_eq!(val, 99));
    }

    #[test]
    fn assert_outcome_ok_panics_on_fault() {
        let outcome: Outcome<i32, String> = Outcome::Fault("err".to_string());
        let result = std::panic::catch_unwind(|| {
            assert_outcome_ok!(outcome);
        });
        assert!(result.is_err());
    }

    #[test]
    fn assert_outcome_err_panics_on_next() {
        let outcome: Outcome<i32, String> = Outcome::Next(42);
        let result = std::panic::catch_unwind(|| {
            assert_outcome_err!(outcome);
        });
        assert!(result.is_err());
    }
}
