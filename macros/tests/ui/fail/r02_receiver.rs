// R02: transition methods with a receiver are rejected.
#![allow(unused_imports)]
use ranvier_core::outcome::Outcome;
use ranvier_macros::transition;

struct Handler;

impl Handler {
    #[transition]
    async fn invalid(&self) -> Outcome<(), String> {
        Outcome::next(())
    }
}

fn main() {}
