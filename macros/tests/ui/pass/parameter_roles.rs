//! Compile-checks the supported transition parameter roles and generated APIs.

#![deny(missing_docs)]
#![allow(dead_code)]
#![deny(warnings)]

use std::convert::Infallible;

use ranvier_core::transition::{ResourceRequirement, Transition};
use ranvier_macros::transition;

struct AppResources;
impl ResourceRequirement for AppResources {}

fn assert_transition<T, From, To, Error, Resources>(_: &T)
where
    T: Transition<From, To, Error = Error, Resources = Resources> + Clone,
    From: Send + 'static,
    To: Send + 'static,
{
}

fn bus_type_witness(_: &mut ranvier_core::bus::Bus) {}

#[transition]
async fn no_parameters() -> ranvier_core::outcome::Outcome<(), Infallible> {
    ranvier_core::outcome::Outcome::next(())
}

#[transition]
async fn unit_input(_input: ()) -> ranvier_core::outcome::Outcome<(), Infallible> {
    ranvier_core::outcome::Outcome::next(())
}

#[transition]
async fn input_only(input: i32) -> ranvier_core::outcome::Outcome<i32, Infallible> {
    ranvier_core::outcome::Outcome::next(input)
}

#[transition]
async fn resources_only(_resources: &AppResources) -> ranvier_core::outcome::Outcome<(), Infallible> {
    ranvier_core::outcome::Outcome::next(())
}

#[transition]
async fn bus_only(_bus: &mut ranvier_core::bus::Bus) -> ranvier_core::outcome::Outcome<(), Infallible> {
    ranvier_core::outcome::Outcome::next(())
}

#[transition]
async fn input_and_resources(input: i32, _resources: &AppResources) -> ranvier_core::outcome::Outcome<i32, Infallible> {
    ranvier_core::outcome::Outcome::next(input)
}

#[transition]
async fn input_and_bus(input: i32, _bus: &mut ranvier_core::bus::Bus) -> ranvier_core::outcome::Outcome<i32, Infallible> {
    ranvier_core::outcome::Outcome::next(input)
}

#[transition]
async fn resources_and_bus(_resources: &AppResources, _bus: &mut ranvier_core::bus::Bus) -> ranvier_core::outcome::Outcome<(), Infallible> {
    ranvier_core::outcome::Outcome::next(())
}

#[transition]
async fn all_parameters(
    input: i32,
    _resources: &AppResources,
    _bus: &mut ranvier_core::bus::Bus,
) -> ranvier_core::outcome::Outcome<i32, Infallible> {
    ranvier_core::outcome::Outcome::next(input)
}

#[transition]
async fn destructuring_input((left, right): (i32, i32)) -> ranvier_core::outcome::Outcome<i32, Infallible> {
    ranvier_core::outcome::Outcome::next(left + right)
}

#[transition(schema)]
async fn schema_metadata(input: String) -> ranvier_core::outcome::Outcome<String, Infallible> {
    ranvier_core::outcome::Outcome::next(input)
}

#[transition(resources = AppResources)]
async fn override_no_parameters() -> ranvier_core::outcome::Outcome<(), Infallible> {
    ranvier_core::outcome::Outcome::next(())
}

#[transition(resources = AppResources)]
async fn override_input(input: i32) -> ranvier_core::outcome::Outcome<i32, Infallible> {
    ranvier_core::outcome::Outcome::next(input)
}

#[transition(resources = AppResources)]
async fn override_bus(_bus: &mut ranvier_core::bus::Bus) -> ranvier_core::outcome::Outcome<(), Infallible> {
    ranvier_core::outcome::Outcome::next(())
}

#[transition(resources = AppResources)]
async fn override_input_and_bus(input: i32, _bus: &mut ranvier_core::bus::Bus) -> ranvier_core::outcome::Outcome<i32, Infallible> {
    ranvier_core::outcome::Outcome::next(input)
}

/// The public transition keeps its source documentation.
#[transition]
#[allow(dead_code)]
pub async fn public_transition(input: String) -> ranvier_core::outcome::Outcome<String, Infallible> {
    ranvier_core::outcome::Outcome::next(input)
}

#[transition]
#[cfg(any())]
async fn removed_by_cfg(_input: MissingInput) -> ranvier_core::outcome::Outcome<MissingOutput, MissingError> {
    unreachable!()
}

fn main() {
    assert_transition::<_, (), (), Infallible, ()>(&no_parameters);
    assert_transition::<_, (), (), Infallible, ()>(&unit_input);
    assert_transition::<_, i32, i32, Infallible, ()>(&input_only);
    assert_transition::<_, (i32, i32), i32, Infallible, ()>(&destructuring_input);
    assert!(schema_metadata.input_schema().is_some());
    assert_transition::<_, (), (), Infallible, AppResources>(&resources_only);
    assert_transition::<_, (), (), Infallible, ()>(&bus_only);
    assert_transition::<_, i32, i32, Infallible, AppResources>(&input_and_resources);
    assert_transition::<_, i32, i32, Infallible, ()>(&input_and_bus);
    assert_transition::<_, (), (), Infallible, AppResources>(&resources_and_bus);
    assert_transition::<_, i32, i32, Infallible, AppResources>(&all_parameters);
    assert_transition::<_, (), (), Infallible, AppResources>(&override_no_parameters);
    assert_transition::<_, i32, i32, Infallible, AppResources>(&override_input);
    assert_transition::<_, (), (), Infallible, AppResources>(&override_bus);
    assert_transition::<_, i32, i32, Infallible, AppResources>(&override_input_and_bus);
    assert_transition::<_, String, String, Infallible, ()>(&public_transition);
}
