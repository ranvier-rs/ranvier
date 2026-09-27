#![allow(unused_imports)]

use ranvier_core::outcome::Outcome;
use ranvier_macros::transition;

#[transition]
async fn generic_input<T>(_input: T) -> Outcome<(), String> {
    Outcome::next(())
}

fn main() {}
