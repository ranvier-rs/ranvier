#![deny(warnings)]

use std::convert::Infallible;

use ranvier_core::transition::Transition;
use ranvier_ordinary_public_api_fixture::{
    assert_crate_scoped_transition,
    transitions::echo,
};

fn assert_public_contract<T>(_: &T)
where
    T: Transition<i32, i32, Error = Infallible, Resources = ()> + Clone,
{
}

fn main() {
    assert_public_contract(&echo);
    assert_crate_scoped_transition();
}
