use ranvier_macros::streaming_transition;

#[streaming_transition(schema)]
async fn invalid_stream() -> Result<impl Stream<Item = String> + Send, std::convert::Infallible> {
    todo!()
}

fn main() {}
