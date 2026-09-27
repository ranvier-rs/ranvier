use ranvier_macros::streaming_transition;

#[streaming_transition]
async fn invalid_stream(input: String) -> String {
    input
}

fn main() {}
