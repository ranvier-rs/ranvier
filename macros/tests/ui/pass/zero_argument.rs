use ranvier_core::outcome::Outcome;
use ranvier_macros::transition;

#[transition]
async fn greet() -> Outcome<String, String> {
    Outcome::next("Hello, Ranvier!".to_string())
}

fn main() {
    let _named_transition = greet;
}
