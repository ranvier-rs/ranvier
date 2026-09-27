// R15: Bus cannot be passed by value.
#![allow(unused_imports)]
use ranvier_core::{bus::Bus, outcome::Outcome};
use ranvier_macros::transition;

#[transition]
async fn invalid(_bus: Bus) -> Outcome<(), String> {
    Outcome::next(())
}

fn main() {}
