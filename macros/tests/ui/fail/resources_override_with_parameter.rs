#![allow(unused_imports)]

use ranvier_core::outcome::Outcome;
use ranvier_core::transition::ResourceRequirement;
use ranvier_macros::transition;

struct AppResources;
impl ResourceRequirement for AppResources {}

#[transition(resources = AppResources)]
async fn conflicting_resource(_resources: &AppResources) -> Outcome<(), String> {
    Outcome::next(())
}

fn main() {}
