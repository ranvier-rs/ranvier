#![allow(unused_imports)]

use ranvier_core::bus::Bus;
use ranvier_core::outcome::Outcome;
use ranvier_macros::transition;

#[transition]
async fn bus_before_input(_bus: &mut Bus, _input: i32) -> Outcome<(), String> {
    Outcome::next(())
}

fn main() {}
