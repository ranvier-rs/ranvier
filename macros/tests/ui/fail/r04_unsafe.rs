// R04: unsafe transition functions are rejected.
#![allow(unused_imports)]
use ranvier_core::outcome::Outcome;
use ranvier_macros::transition;

#[transition]
async unsafe fn invalid() -> Outcome<(), String> {
    Outcome::next(())
}

fn main() {}
