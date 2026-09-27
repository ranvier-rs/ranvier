// R29: removed diagram coordinate attributes are rejected.
#![allow(unused_imports)]
use ranvier_core::outcome::Outcome;
use ranvier_macros::transition;

#[transition(y = 20)]
async fn invalid() -> Outcome<(), String> {
    Outcome::next(())
}

fn main() {}
