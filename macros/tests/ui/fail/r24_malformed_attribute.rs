// R24: malformed attribute values are rejected.
#![allow(unused_imports)]
use ranvier_core::outcome::Outcome;
use ranvier_macros::transition;

#[transition(resources =)]
async fn invalid() -> Outcome<(), String> {
    Outcome::next(())
}

fn main() {}
