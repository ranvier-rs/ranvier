#[transition]
async fn greet(input: String) -> Outcome<String, String> {
    Outcome::Next(format!("Hello, {input}!"))
}
