use std::convert::Infallible;

use ranvier_core::{bus::BusTypeRef, outcome::Outcome, transition::Transition};
use ranvier_macros::transition;

struct AllowedBus;
struct DeniedBus;
struct AlsoAllowedBus;

#[transition(schema)]
async fn schema_metadata(input: String) -> Outcome<String, Infallible> {
    Outcome::next(input)
}

#[transition(bus_allow = [AllowedBus])]
async fn allow_only() -> Outcome<(), Infallible> {
    Outcome::next(())
}

#[transition(bus_deny = [DeniedBus])]
async fn deny_only() -> Outcome<(), Infallible> {
    Outcome::next(())
}

#[transition(bus_allow = [AllowedBus, AlsoAllowedBus], bus_deny = [DeniedBus])]
async fn allow_and_deny() -> Outcome<(), Infallible> {
    Outcome::next(())
}

#[test]
fn schema_attribute_returns_generated_schema_value() {
    let schema = schema_metadata
        .input_schema()
        .expect("schema attribute should populate input schema metadata");

    assert_eq!(
        schema.get("type").and_then(|value| value.as_str()),
        Some("string")
    );
}

#[test]
fn bus_allow_metadata_contains_the_declared_type() {
    let policy = allow_only
        .bus_access_policy()
        .expect("bus_allow should populate policy metadata");

    assert_eq!(policy.allow, Some(vec![BusTypeRef::of::<AllowedBus>()]));
    assert!(policy.deny.is_empty());
}

#[test]
fn bus_deny_metadata_contains_the_declared_type() {
    let policy = deny_only
        .bus_access_policy()
        .expect("bus_deny should populate policy metadata");

    assert!(policy.allow.is_none());
    assert_eq!(policy.deny, vec![BusTypeRef::of::<DeniedBus>()]);
}

#[test]
fn disjoint_bus_allow_and_deny_metadata_are_preserved() {
    let policy = allow_and_deny
        .bus_access_policy()
        .expect("bus_allow and bus_deny should populate policy metadata");

    assert_eq!(
        policy.allow,
        Some(vec![
            BusTypeRef::of::<AllowedBus>(),
            BusTypeRef::of::<AlsoAllowedBus>()
        ])
    );
    assert_eq!(policy.deny, vec![BusTypeRef::of::<DeniedBus>()]);
}

const LOCATED_LINE: u32 = line!() + 2;
#[transition]
async fn located() -> Outcome<(), Infallible> {
    Outcome::next(())
}

#[test]
fn definition_source_points_at_the_annotated_function() {
    let source = located
        .definition_source()
        .expect("#[transition] should record its definition");
    assert_eq!(source.file, file!());
    assert_eq!(source.line, LOCATED_LINE);
    assert!(source.column.is_none());
}

#[cfg(feature = "streaming")]
mod streaming_definition {
    use std::{convert::Infallible, pin::Pin};

    use ranvier_core::{__macro_support::futures_core::Stream, streaming::StreamingTransition};
    use ranvier_macros::streaming_transition;

    const STREAM_LINE: u32 = line!() + 2;
    #[streaming_transition]
    async fn located_stream(
        input: i32,
    ) -> Result<Pin<Box<dyn Stream<Item = i32> + Send>>, Infallible> {
        let _ = input;
        unreachable!("only metadata is inspected")
    }

    #[test]
    fn streaming_definition_source_points_at_the_annotated_function() {
        let source = located_stream
            .definition_source()
            .expect("#[streaming_transition] should record its definition");
        assert_eq!(source.file, file!());
        assert_eq!(source.line, STREAM_LINE);
    }
}
