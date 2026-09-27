#[test]
fn transition_macro_contract() {
    let cases = trybuild::TestCases::new();
    cases.pass("tests/ui/pass/*.rs");
    cases.compile_fail("tests/ui/fail/*.rs");
}

#[cfg(feature = "streaming")]
#[test]
fn streaming_transition_macro_contract() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/streaming/fail/*.rs");
}

#[cfg(not(feature = "streaming"))]
#[test]
fn streaming_transition_macro_requires_feature() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/streaming/feature_off/*.rs");
}
