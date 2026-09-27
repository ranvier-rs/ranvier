// R12: a borrowed value cannot serve as an owned transition input.
#![allow(unused_imports)]
use ranvier_core::outcome::Outcome;
use ranvier_macros::transition;

#[transition]
async fn invalid(_input: &'static String) -> Outcome<(), String> {
    Outcome::next(())
}

fn main() {}
