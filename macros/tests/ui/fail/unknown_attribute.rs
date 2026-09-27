#![allow(unused_imports)]

use ranvier_core::outcome::Outcome;
use ranvier_macros::transition;

#[transition(scheam)]
async fn greet() -> Outcome<String, String> {
    Outcome::next("Hello, Ranvier!".to_string())
}

fn main() {}
