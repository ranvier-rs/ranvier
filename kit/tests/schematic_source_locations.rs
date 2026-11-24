//! Schematic source metadata (macro contract section 5): a node records where
//! its transition is defined and, separately, where it is attached to the Axon.

use async_trait::async_trait;
use ranvier_core::bus::Bus;
use ranvier_core::outcome::Outcome;
use ranvier_core::schematic::{NodeKind, SCHEMA_VERSION, SourceLocation};
use ranvier_core::transition::Transition;
use ranvier_macros::transition;
use ranvier_runtime::Axon;

const DEFINED_LINE: u32 = line!() + 2;
#[transition]
async fn defined_step(input: i32) -> Outcome<i32, String> {
    Outcome::next(input + 1)
}

#[derive(Clone)]
struct ManualStep;

#[async_trait]
impl Transition<i32, i32> for ManualStep {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: i32,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<i32, Self::Error> {
        Outcome::next(input)
    }
}

fn assert_location(location: Option<&SourceLocation>, line: u32) {
    let location = location.expect("location should be recorded");
    assert_eq!(location.file, file!());
    assert_eq!(location.line, line);
}

#[test]
fn macro_transition_reports_its_definition() {
    assert_location(defined_step.definition_source().as_ref(), DEFINED_LINE);
}

#[test]
fn nodes_record_definition_and_attachment_separately() {
    let ingress_line = line!() + 1;
    let axon = Axon::<i32, i32, String>::new("located")
        .then(defined_step)
        .then(ManualStep);
    let attach_macro_line = ingress_line + 1;
    let attach_manual_line = ingress_line + 2;

    let nodes = &axon.schematic.nodes;
    assert_eq!(nodes.len(), 3);

    assert!(matches!(nodes[0].kind, NodeKind::Ingress));
    assert!(nodes[0].definition_source.is_none());
    assert_location(nodes[0].attachment_source.as_ref(), ingress_line);

    assert_location(nodes[1].definition_source.as_ref(), DEFINED_LINE);
    assert_location(nodes[1].attachment_source.as_ref(), attach_macro_line);

    assert!(nodes[2].definition_source.is_none());
    assert_location(nodes[2].attachment_source.as_ref(), attach_manual_line);
}

#[test]
fn exported_schematic_uses_version_2_and_no_legacy_field() {
    let axon = Axon::<i32, i32, String>::new("exported").then(defined_step);
    let json = serde_json::to_value(&axon.schematic).unwrap();
    assert_eq!(json["schema_version"], SCHEMA_VERSION);
    assert_eq!(SCHEMA_VERSION, "2.0");
    for node in json["nodes"].as_array().unwrap() {
        assert!(node.get("source_location").is_none());
        assert!(node.get("attachment_source").is_some());
    }
    assert!(json["nodes"][1].get("definition_source").is_some());
}
