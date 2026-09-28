#![deny(warnings)]

use std::{convert::Infallible, pin::Pin, task::{Context, Poll}};

use ranvier_core::__macro_support::futures_core::Stream;

struct Values(std::vec::IntoIter<i32>);

impl Stream for Values {
    type Item = i32;

    fn poll_next(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(self.0.next())
    }
}

/// The transition and its generated type are visible to their parent only.
pub mod parent {
    use super::Infallible;

    pub mod child {
        use super::super::{Infallible, Values};
        use ranvier_macros::streaming_transition;

        #[streaming_transition]
        pub(super) async fn parent_only(
            input: i32,
        ) -> Result<
            impl ranvier_core::__macro_support::futures_core::Stream<Item = i32> + Send,
            Infallible,
        > {
            Ok(Values(vec![input].into_iter()))
        }
    }

    pub fn assert_parent_can_use_transition() {
        let transition = child::parent_only;
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
}

/// A sibling scope must not inherit `pub(super)` visibility.
pub mod sibling {
    pub fn access_is_rejected() {
        let _transition = super::parent::child::parent_only;
        let _transition_type = super::parent::child::__ranvier_stream_transition_parent_only;
    }
}

fn main() {
    parent::assert_parent_can_use_transition();
    sibling::access_is_rejected();
}
