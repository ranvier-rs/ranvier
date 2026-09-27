//! Compile-checks streaming transition return forms without executing the generated binary.

#![allow(dead_code)]
#![deny(warnings)]

use std::{
    convert::Infallible,
    pin::Pin,
    task::{Context, Poll},
};

use ranvier_core::{
    __macro_support::futures_core::Stream, streaming::StreamingTransition,
    transition::ResourceRequirement,
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
impl ResourceRequirement for AppResources {}

#[streaming_transition]
async fn all_parameter_roles(
    input: (i32, i32),
    _resources: &AppResources,
    _bus: &mut ranvier_core::bus::Bus,
) -> Result<impl Stream<Item = i32> + Send, Infallible> {
    Ok(Values(vec![input.0 + input.1].into_iter()))
}

fn assert_streaming_transition<T, From, Item, Error, Resources>(_: &T)
where
    T: StreamingTransition<From, Item = Item, Error = Error, Resources = Resources> + Clone,
    From: Send + 'static,
    Item: Send + 'static,
    Error: Send + Sync + std::fmt::Debug + 'static,
    Resources: ResourceRequirement,
{
}

fn main() {
    assert_streaming_transition::<_, i32, i32, Infallible, ()>(&impl_stream);
    assert_streaming_transition::<_, i32, i32, Infallible, ()>(&pinned_boxed_stream);
    assert_streaming_transition::<_, i32, i32, Infallible, ()>(&qualified_pinned_boxed_stream);
    assert_streaming_transition::<_, (i32, i32), i32, Infallible, AppResources>(
        &all_parameter_roles,
    );
}
