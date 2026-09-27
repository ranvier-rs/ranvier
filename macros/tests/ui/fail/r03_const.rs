// R03: const transition functions are rejected.
#![allow(unused_imports)]
use ranvier_core::outcome::Outcome;
use ranvier_macros::transition;

#[transition]
const async fn invalid() -> Outcome<(), String> {
    Outcome::next(())
}

fn main() {}
