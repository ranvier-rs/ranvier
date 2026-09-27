#![allow(unused_imports)]

use ranvier_core::outcome::Outcome;
use ranvier_macros::transition;

#[transition]
#[inline]
async fn inline_transition() -> Outcome<(), String> {
    Outcome::next(())
}

fn main() {}
