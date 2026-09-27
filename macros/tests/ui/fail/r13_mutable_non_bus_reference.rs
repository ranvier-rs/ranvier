// R13: mutable non-Bus references are rejected.
#![allow(unused_imports)]
use ranvier_core::outcome::Outcome;
use ranvier_macros::transition;

#[transition]
async fn invalid(_resources: &mut String) -> Outcome<(), String> {
    Outcome::next(())
}

fn main() {}
