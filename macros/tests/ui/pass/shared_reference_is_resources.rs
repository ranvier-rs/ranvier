#![allow(dead_code)]

use std::convert::Infallible;

use ranvier_core::{outcome::Outcome, transition::Transition};
use ranvier_macros::transition;

#[transition]
async fn named_like_input(_input: &String) -> Outcome<(), Infallible> {
    Outcome::next(())
}

fn assert_resource_role<T>(_: &T)
where
    T: Transition<(), (), Error = Infallible, Resources = String> + Clone,
{
}

fn main() {
    assert_resource_role(&named_like_input);
}
