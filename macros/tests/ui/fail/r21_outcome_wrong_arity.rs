// R21: ordinary Outcome requires exactly two type arguments.
#![allow(unused_imports)]
use ranvier_core::outcome::Outcome;
use ranvier_macros::transition;

#[transition]
async fn invalid() -> Outcome<String> {
    unreachable!()
}

fn main() {}
