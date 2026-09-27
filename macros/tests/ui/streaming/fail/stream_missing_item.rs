use ranvier_macros::streaming_transition;

#[streaming_transition]
async fn invalid_stream(input: String) -> Result<impl Stream + Send, std::convert::Infallible> {
    let _ = input;
    todo!()
}

fn main() {}
