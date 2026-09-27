// R25: duplicate types in an attribute list are rejected.
#![allow(unused_imports)]
use ranvier_core::outcome::Outcome;
use ranvier_macros::transition;

#[transition(bus_allow = [String, String])]
async fn invalid() -> Outcome<(), String> {
    Outcome::next(())
}

fn main() {}
