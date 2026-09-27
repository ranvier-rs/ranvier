// R05: extern transition functions are rejected.
#![allow(unused_imports)]
use ranvier_core::outcome::Outcome;
use ranvier_macros::transition;

#[transition]
async extern "C" fn invalid() -> Outcome<(), String> {
    Outcome::next(())
}

fn main() {}
