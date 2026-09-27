#![allow(unused_imports)]

use ranvier_core::outcome::Outcome;
use ranvier_macros::transition;

#[transition]
fn not_async() -> Outcome<(), String> {
    Outcome::next(())
}

fn main() {}
