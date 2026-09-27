#![allow(unused_imports)]

use ranvier_core::outcome::Outcome;
use ranvier_macros::transition;

#[transition]
async fn duplicate_inputs(_first: i32, _second: i32) -> Outcome<(), String> {
    Outcome::next(())
}

fn main() {}
