// R28: removed diagram coordinate attributes are rejected.
#![allow(unused_imports)]
use ranvier_core::outcome::Outcome;
use ranvier_macros::transition;

#[transition(x = 10)]
async fn invalid() -> Outcome<(), String> {
    Outcome::next(())
}

fn main() {}
