use ranvier_macros::streaming_transition;

#[streaming_transition]
async fn invalid_stream(input: String) -> Result<impl Stream<Item = String> + Send> {
    let _ = input;
    todo!()
}

fn main() {}
