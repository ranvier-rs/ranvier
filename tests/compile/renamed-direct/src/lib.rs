//! Compile-only proof for renamed direct dependencies without macro-support crates.

use core_alias::transition::Transition;

#[derive(Clone)]
struct RequestContext;

#[macro_alias::transition(schema, bus_allow = [RequestContext])]
async fn renamed_transition(
    input: String,
    _bus: &mut core_alias::bus::Bus,
) -> core_alias::outcome::Outcome<String, std::convert::Infallible> {
    core_alias::outcome::Outcome::next(input)
}

fn assert_clone_transition<T>(_: &T)
where
    T: Transition<String, String, Error = std::convert::Infallible, Resources = ()> + Clone,
{
}

pub fn compile_renamed_dependency_paths() {
    assert_clone_transition(&renamed_transition);
    assert!(renamed_transition.input_schema().is_some());
}
