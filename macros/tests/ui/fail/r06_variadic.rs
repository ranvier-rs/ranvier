// R06: variadic transition functions are rejected.
#![allow(unused_imports)]
use ranvier_core::outcome::Outcome;
use ranvier_macros::transition;

#[transition]
async extern "C" fn invalid(_input: i32, _: ...) -> Outcome<(), String> {
    Outcome::next(())
}

fn main() {}
