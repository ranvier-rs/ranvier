use ranvier_macros::streaming_transition;

#[streaming_transition(bus_allow = [String])]
async fn invalid_stream() -> Result<impl Stream<Item = String> + Send, std::convert::Infallible> {
    todo!()
}

fn main() {}
