use std::{pin::Pin, task::{Context, Poll}};

struct Values(std::vec::IntoIter<i32>);

impl ranvier_core::__macro_support::futures_core::Stream for Values {
    type Item = i32;

    fn poll_next(
        mut self: Pin<&mut Self>,
        _context: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        Poll::Ready(self.0.next())
    }
}

mod private_api {
    use super::Values;
    use ranvier_macros::streaming_transition;

    #[streaming_transition]
    async fn private_stream(
        input: i32,
    ) -> Result<
        impl ranvier_core::__macro_support::futures_core::Stream<Item = i32> + Send,
        std::convert::Infallible,
    > {
        Ok(Values(vec![input].into_iter()))
    }
}

fn main() {
    let _transition = private_api::private_stream;
}
