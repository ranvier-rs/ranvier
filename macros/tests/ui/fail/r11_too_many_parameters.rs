// R11: four or more transition parameters are rejected before role checks.
#![allow(unused_imports)]
use ranvier_core::{bus::Bus, outcome::Outcome, transition::ResourceRequirement};
use ranvier_macros::transition;

struct AppResources;
impl ResourceRequirement for AppResources {}

#[transition]
async fn invalid(
    _input: i32,
    _resources: &AppResources,
    _bus: &mut Bus,
    _extra: i64,
) -> Outcome<(), String> {
    Outcome::next(())
}

fn main() {}
