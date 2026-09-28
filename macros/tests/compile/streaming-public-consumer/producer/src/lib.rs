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

    /// A crate-visible transition used to check downstream privacy.
    #[streaming_transition]
    pub(crate) async fn crate_scoped(
        input: i32,
    ) -> Result<
        impl ranvier_core::__macro_support::futures_core::Stream<Item = i32> + Send,
        Infallible,
    > {
        Ok(Values(vec![input].into_iter()))
    }
}

/// Verifies that the producer crate can use its crate-visible transition.
pub fn assert_crate_scoped_transition() {
    let transition = transitions::crate_scoped;
    fn assert_contract<
        T: ranvier_core::streaming::StreamingTransition<
                i32,
                Item = i32,
                Error = Infallible,
                Resources = (),
            >,
    >(
        _: &T,
    ) {
    }
    assert_contract(&transition);
}
