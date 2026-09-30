//! A14: named struct destructuring remains a valid owned input pattern.

#![allow(dead_code)]

use std::convert::Infallible;

use ranvier_core::{outcome::Outcome, transition::Transition};
use ranvier_macros::transition;

struct Request {
    value: i32,
}

#[transition]
async fn destructure_request(Request { value }: Request) -> Outcome<i32, Infallible> {
    Outcome::next(value)
}

fn assert_transition<T>(_: &T)
where
    T: Transition<Request, i32, Error = Infallible, Resources = ()> + Clone,
{
}

fn main() {
    assert_transition(&destructure_request);
}
