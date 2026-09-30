#![allow(unused_imports)]

use ranvier_core::outcome::Outcome;
use ranvier_macros::transition;

struct AppResources;

#[transition(resources = AppResources)]
async fn conflicting_resource(_resources: &AppResources) -> Outcome<(), String> {
    Outcome::next(())
}

fn main() {}
