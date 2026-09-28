//! Producer package for the downstream ordinary transition API contract.

#![deny(missing_docs, warnings)]

/// Ordinary transition values exported by this fixture library.
pub mod transitions {
    use std::convert::Infallible;

    use ranvier_core::outcome::Outcome;
    use ranvier_macros::transition;

    /// Returns its input as the next transition value.
    #[transition]
    pub async fn echo(input: i32) -> Outcome<i32, Infallible> {
        Outcome::next(input)
    }
}
