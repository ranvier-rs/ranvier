//! Producer package for the downstream streaming visibility contract.

#![deny(missing_docs, warnings)]

use std::{
    convert::Infallible,
    pin::Pin,
    task::{Context, Poll},
};

use ranvier_core::__macro_support::futures_core::Stream;

struct Values(std::vec::IntoIter<i32>);

impl Stream for Values {
    type Item = i32;

    fn poll_next(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(self.0.next())
    }
}

/// Streaming transition values exported by this fixture library.
pub mod transitions {
    use super::{Infallible, Values};
    use ranvier_macros::streaming_transition;

    /// Emits the input as one item for a downstream crate.
    #[streaming_transition]
    pub async fn emit_one(
        input: i32,
    ) -> Result<
        impl ranvier_core::__macro_support::futures_core::Stream<Item = i32> + Send,
        Infallible,
    > {
        Ok(Values(vec![input].into_iter()))
    }
}
