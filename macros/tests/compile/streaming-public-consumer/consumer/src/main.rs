#![deny(warnings)]

use std::convert::Infallible;

use ranvier_core::streaming::StreamingTransition;
use ranvier_streaming_public_api_fixture::transitions::emit_one;

fn assert_public_contract<T>(_: &T)
where
    T: StreamingTransition<i32, Item = i32, Error = Infallible, Resources = ()> + Clone,
{
}

fn main() {
    assert_public_contract(&emit_one);
}
