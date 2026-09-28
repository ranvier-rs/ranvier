//! Compile-only proof for renamed direct dependencies without macro-support crates.

use std::{pin::Pin, task::{Context, Poll}};

use core_alias::{
    __macro_support::futures_core::Stream,
    streaming::StreamingTransition,
    transition::{ResourceRequirement, Transition},
};

#[derive(Clone)]
struct RequestContext;

#[macro_alias::transition(schema, bus_allow = [RequestContext])]
async fn renamed_transition(
    input: String,
    _bus: &mut core_alias::bus::Bus,
) -> core_alias::outcome::Outcome<String, std::convert::Infallible> {
    core_alias::outcome::Outcome::next(input)
}

fn assert_clone_transition<T>(_: &T)
where
    T: Transition<String, String, Error = std::convert::Infallible, Resources = ()> + Clone,
{
}

pub fn compile_renamed_dependency_paths() {
    assert_clone_transition(&renamed_transition);
    assert!(renamed_transition.input_schema().is_some());
    assert_clone_streaming_transition(&renamed_streaming_transition);
}

struct AppResources;

impl ResourceRequirement for AppResources {}

struct OneItemStream(Option<i32>);

impl Stream for OneItemStream {
    type Item = i32;

    fn poll_next(
        mut self: Pin<&mut Self>,
        _context: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        Poll::Ready(self.0.take())
    }
}

#[macro_alias::streaming_transition]
async fn renamed_streaming_transition(
    input: i32,
    _resources: &AppResources,
    _bus: &mut core_alias::bus::Bus,
) -> Result<impl Stream<Item = i32> + Send, String> {
    Ok(OneItemStream(Some(input)))
}

fn assert_clone_streaming_transition<T>(_: &T)
where
    T: StreamingTransition<i32, Item = i32, Error = String, Resources = AppResources> + Clone,
{
}
