//! Compile-checks streaming transition return forms without executing the generated binary.

#![allow(dead_code)]
#![deny(warnings)]

use std::{
    convert::Infallible,
    pin::Pin,
    task::{Context, Poll},
};

use ranvier_core::{
    __macro_support::futures_core::Stream, bus::Bus, streaming::StreamingTransition,
};
use ranvier_macros::streaming_transition;

struct Values(std::vec::IntoIter<i32>);

impl Stream for Values {
    type Item = i32;

    fn poll_next(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(self.0.next())
    }
}

#[streaming_transition]
async fn impl_stream(input: i32) -> Result<impl Stream<Item = i32> + Send, Infallible> {
    Ok(Values(vec![input].into_iter()))
}

#[streaming_transition]
async fn pinned_boxed_stream(
    input: i32,
) -> Result<Pin<Box<dyn Stream<Item = i32> + Send>>, Infallible> {
    Ok(Box::pin(Values(vec![input].into_iter())))
}

#[streaming_transition]
async fn qualified_pinned_boxed_stream(
    input: i32,
) -> std::result::Result<
    std::pin::Pin<
        std::boxed::Box<dyn ranvier_core::__macro_support::futures_core::Stream<Item = i32> + Send>,
    >,
    Infallible,
> {
    Ok(Box::pin(Values(vec![input].into_iter())))
}

struct AppResources;

// Keep the imported type alias observable after the macro consumes its source signature.
fn bus_type_witness(_: &mut Bus) {}

struct Pair {
    left: i32,
    right: i32,
}

// A01: no input, resources, or Bus role.
#[streaming_transition]
async fn no_parameters() -> Result<impl Stream<Item = i32> + Send, Infallible> {
    Ok(Values(vec![0].into_iter()))
}

// A02: owned input only.
#[streaming_transition]
async fn input_only(input: i32) -> Result<impl Stream<Item = i32> + Send, Infallible> {
    Ok(Values(vec![input].into_iter()))
}

// A03: shared resource reference only.
#[streaming_transition]
async fn resources_only(
    _resources: &AppResources,
) -> Result<impl Stream<Item = i32> + Send, Infallible> {
    Ok(Values(vec![0].into_iter()))
}

// A04: imported Bus name with a mutable reference only.
#[streaming_transition]
async fn bus_only(_bus: &mut Bus) -> Result<impl Stream<Item = i32> + Send, Infallible> {
    Ok(Values(vec![0].into_iter()))
}

// A13: qualified Bus path with a mutable reference only.
#[streaming_transition]
async fn qualified_bus_only(
    _bus: &mut ranvier_core::bus::Bus,
) -> Result<impl Stream<Item = i32> + Send, Infallible> {
    Ok(Values(vec![0].into_iter()))
}

// A05: owned input followed by resources.
#[streaming_transition]
async fn input_and_resources(
    input: i32,
    _resources: &AppResources,
) -> Result<impl Stream<Item = i32> + Send, Infallible> {
    Ok(Values(vec![input].into_iter()))
}

// A06: owned input followed by Bus.
#[streaming_transition]
async fn input_and_bus(
    input: i32,
    _bus: &mut ranvier_core::bus::Bus,
) -> Result<impl Stream<Item = i32> + Send, Infallible> {
    Ok(Values(vec![input].into_iter()))
}

// A07: resources followed by Bus.
#[streaming_transition]
async fn resources_and_bus(
    _resources: &AppResources,
    _bus: &mut ranvier_core::bus::Bus,
) -> Result<impl Stream<Item = i32> + Send, Infallible> {
    Ok(Values(vec![0].into_iter()))
}

// A08: all three parameter roles in their required order.
#[streaming_transition]
async fn all_parameter_roles(
    input: (i32, i32),
    _resources: &AppResources,
    _bus: &mut ranvier_core::bus::Bus,
) -> Result<impl Stream<Item = i32> + Send, Infallible> {
    Ok(Values(vec![input.0 + input.1].into_iter()))
}

// A09: resources override with no source parameter.
#[streaming_transition(resources = AppResources)]
async fn override_no_parameters() -> Result<impl Stream<Item = i32> + Send, Infallible> {
    Ok(Values(vec![0].into_iter()))
}

// A10: resources override plus owned input.
#[streaming_transition(resources = AppResources)]
async fn override_input(input: i32) -> Result<impl Stream<Item = i32> + Send, Infallible> {
    Ok(Values(vec![input].into_iter()))
}

// A11: resources override plus Bus.
#[streaming_transition(resources = AppResources)]
async fn override_bus(
    _bus: &mut ranvier_core::bus::Bus,
) -> Result<impl Stream<Item = i32> + Send, Infallible> {
    Ok(Values(vec![0].into_iter()))
}

// A12: resources override plus owned input and Bus.
#[streaming_transition(resources = AppResources)]
async fn override_input_and_bus(
    input: i32,
    _bus: &mut ranvier_core::bus::Bus,
) -> Result<impl Stream<Item = i32> + Send, Infallible> {
    Ok(Values(vec![input].into_iter()))
}

// A14: an ordinary destructuring pattern remains an owned input role.
#[streaming_transition]
async fn destructuring_input(
    Pair { left, right }: Pair,
) -> Result<impl Stream<Item = i32> + Send, Infallible> {
    Ok(Values(vec![left + right].into_iter()))
}

fn assert_streaming_transition<T, From, Item, Error, Resources>(_: &T)
where
    T: StreamingTransition<From, Item = Item, Error = Error, Resources = Resources> + Clone,
    From: Send + 'static,
    Item: Send + 'static,
    Error: Send + Sync + std::fmt::Debug + 'static,
    Resources: Send + Sync + 'static,
{
}

fn main() {
    assert_streaming_transition::<_, (), i32, Infallible, ()>(&no_parameters);
    assert_streaming_transition::<_, i32, i32, Infallible, ()>(&input_only);
    assert_streaming_transition::<_, (), i32, Infallible, AppResources>(&resources_only);
    assert_streaming_transition::<_, (), i32, Infallible, ()>(&bus_only);
    assert_streaming_transition::<_, (), i32, Infallible, ()>(&qualified_bus_only);
    assert_streaming_transition::<_, i32, i32, Infallible, AppResources>(&input_and_resources);
    assert_streaming_transition::<_, i32, i32, Infallible, ()>(&input_and_bus);
    assert_streaming_transition::<_, (), i32, Infallible, AppResources>(&resources_and_bus);
    assert_streaming_transition::<_, i32, i32, Infallible, ()>(&impl_stream);
    assert_streaming_transition::<_, i32, i32, Infallible, ()>(&pinned_boxed_stream);
    assert_streaming_transition::<_, i32, i32, Infallible, ()>(&qualified_pinned_boxed_stream);
    assert_streaming_transition::<_, (i32, i32), i32, Infallible, AppResources>(
        &all_parameter_roles,
    );
    assert_streaming_transition::<_, (), i32, Infallible, AppResources>(&override_no_parameters);
    assert_streaming_transition::<_, i32, i32, Infallible, AppResources>(&override_input);
    assert_streaming_transition::<_, (), i32, Infallible, AppResources>(&override_bus);
    assert_streaming_transition::<_, i32, i32, Infallible, AppResources>(&override_input_and_bus);
    assert_streaming_transition::<_, Pair, i32, Infallible, ()>(&destructuring_input);
}
