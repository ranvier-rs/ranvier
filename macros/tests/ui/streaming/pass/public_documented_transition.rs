//! Public streaming macro symbols must remain usable and documented.

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

/// Public streaming transition fixture module.
pub mod public_api {
    use super::{Infallible, Values};
    use ranvier_macros::streaming_transition;

    /// Produces the input as a single stream item.
    #[streaming_transition]
    pub async fn stream_values(
        input: i32,
    ) -> Result<impl super::Stream<Item = i32> + Send, Infallible> {
        Ok(Values(vec![input].into_iter()))
    }
}

fn main() {
    assert_streaming_transition::<_, i32, i32, Infallible, ()>(&public_api::stream_values);
}
