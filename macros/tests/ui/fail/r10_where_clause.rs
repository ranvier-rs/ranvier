// R10: transition functions with a where clause are rejected.
#![allow(unused_imports)]
use ranvier_core::outcome::Outcome;
use ranvier_macros::transition;

#[transition]
async fn invalid<T>(_input: T) -> Outcome<(), String>
where
    T: Send,
{
    Outcome::next(())
}

fn main() {}
