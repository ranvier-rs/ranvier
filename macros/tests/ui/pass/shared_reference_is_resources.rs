#![allow(dead_code)]

use std::convert::Infallible;

use ranvier_core::{outcome::Outcome, transition::{ResourceRequirement, Transition}};
use ranvier_macros::transition;

struct BorrowedDomainValue;
impl ResourceRequirement for BorrowedDomainValue {}

#[transition]
async fn named_like_input(_input: &BorrowedDomainValue) -> Outcome<(), Infallible> {
    Outcome::next(())
}

fn assert_resource_role<T>(_: &T)
where
    T: Transition<(), (), Error = Infallible, Resources = BorrowedDomainValue> + Clone,
{
}

fn main() {
    assert_resource_role(&named_like_input);
}
