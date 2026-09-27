// R23: duplicate attribute keys are rejected.
#![allow(unused_imports)]
use ranvier_core::outcome::Outcome;
use ranvier_macros::transition;

#[transition(resources = (), resources = ())]
async fn invalid() -> Outcome<(), String> {
    Outcome::next(())
}

fn main() {}
