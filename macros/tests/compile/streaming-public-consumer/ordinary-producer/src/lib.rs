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

    /// A crate-scoped transition used to verify downstream visibility.
    #[transition]
    pub(crate) async fn crate_scoped(input: i32) -> Outcome<i32, Infallible> {
        Outcome::next(input)
    }
}

/// Verifies the producer can use its crate-scoped transition contract.
pub fn assert_crate_scoped_transition() {
    use std::convert::Infallible;
    use ranvier_core::transition::Transition;

    fn assert_contract<T: Transition<i32, i32, Error = Infallible, Resources = ()>>(_: &T) {}

    assert_contract(&transitions::crate_scoped);
}
