// R26: bus allow and deny lists must be disjoint.
#![allow(unused_imports)]
use ranvier_core::outcome::Outcome;
use ranvier_macros::transition;

#[transition(bus_allow = [String], bus_deny = [String])]
async fn invalid() -> Outcome<(), String> {
    Outcome::next(())
}

fn main() {}
