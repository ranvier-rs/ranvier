#![allow(unused_imports)]

use ranvier_core::outcome::Outcome;
use ranvier_macros::transition;

#[transition]
async fn invalid_return() -> Result<String, String> {
    Ok(String::new())
}

fn main() {}
