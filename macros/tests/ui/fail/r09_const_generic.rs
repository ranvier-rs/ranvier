// R09: const-generic transition functions are rejected.
#![allow(unused_imports)]
use ranvier_core::outcome::Outcome;
use ranvier_macros::transition;

#[transition]
async fn invalid<const N: usize>(_input: [u8; N]) -> Outcome<(), String> {
    Outcome::next(())
}

fn main() {}
