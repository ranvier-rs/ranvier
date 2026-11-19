// R12: a shared resource whose referent does not meet the native resource bounds.
// Primitive types keep the expected diagnostics identical across supported rustc
// versions (older compilers print `std::string::String` where newer print `String`).
use std::rc::Rc;

use ranvier_core::outcome::Outcome;
use ranvier_macros::transition;

#[transition]
async fn invalid(_resources: &Rc<u32>) -> Outcome<(), u32> {
    Outcome::next(())
}

fn main() {}
