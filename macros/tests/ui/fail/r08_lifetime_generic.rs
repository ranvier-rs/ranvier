// R08: lifetime-generic transition functions are rejected.
#![allow(unused_imports)]
use ranvier_core::outcome::Outcome;
use ranvier_macros::transition;

#[transition]
async fn invalid<'a>(_input: &'a String) -> Outcome<(), String> {
    Outcome::next(())
}

fn main() {}
