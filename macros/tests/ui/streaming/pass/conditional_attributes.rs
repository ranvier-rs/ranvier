//! Conditional attributes must not leave an unusable streaming symbol.

#![deny(missing_docs)]
#![deny(warnings)]

use std::{
    convert::Infallible,
    pin::Pin,
    task::{Context, Poll},
};

use ranvier_core::{
    __macro_support::futures_core::Stream, streaming::StreamingTransition,
};

struct Values(std::vec::IntoIter<i32>);

impl Stream for Values {
    type Item = i32;

    fn poll_next(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(self.0.next())
    }
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

/// Public APIs whose documentation and conditional availability are checked.
pub mod conditional_api {
    use super::{assert_streaming_transition, Infallible, Values};
    use ranvier_macros::streaming_transition;

    #[cfg_attr(all(), doc = "This documentation is attached through cfg_attr.")]
    #[streaming_transition]
    pub async fn documented_stream(
        input: i32,
    ) -> Result<impl super::Stream<Item = i32> + Send, Infallible> {
        Ok(Values(vec![input].into_iter()))
    }

    #[streaming_transition]
    #[cfg(any())]
    async fn gated_by_cfg() -> Result<impl super::Stream<Item = i32> + Send, Infallible> {
        panic!("a cfg-disabled transition must not leave generated symbols")
    }

    #[allow(non_upper_case_globals)]
    const gated_by_cfg: () = ();

    /// Exercises the retained conditional API and its sentinel.
    pub fn assert_contract() {
        assert_streaming_transition::<_, i32, i32, Infallible, ()>(&documented_stream);
        let _ = gated_by_cfg;
    }
}

fn main() {
    conditional_api::assert_contract();
}
