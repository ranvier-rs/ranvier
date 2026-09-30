// R12: a shared resource whose referent does not meet the native resource bounds.
use std::rc::Rc;

use ranvier_core::outcome::Outcome;
use ranvier_macros::transition;

#[transition]
async fn invalid(_resources: &Rc<String>) -> Outcome<(), String> {
    Outcome::next(())
}

fn main() {}
