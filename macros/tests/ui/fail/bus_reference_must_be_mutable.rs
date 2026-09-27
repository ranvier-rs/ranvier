#![allow(unused_imports)]

use ranvier_core::bus::Bus;
use ranvier_core::outcome::Outcome;
use ranvier_macros::transition;

#[transition]
async fn invalid_bus(_bus: &Bus) -> Outcome<(), String> {
    Outcome::next(())
}

fn main() {}
