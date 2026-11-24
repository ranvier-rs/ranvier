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

const COMPENSATE_LINE: u32 = line!() + 2;
#[transition]
async fn compensate_step(input: i32) -> Outcome<(), String> {
    let _ = input;
    Outcome::next(())
}

fn node_by_label<'a>(
    nodes: &'a [ranvier_core::schematic::Node],
    label: &str,
) -> &'a ranvier_core::schematic::Node {
    nodes
        .iter()
        .find(|node| node.label == label)
        .unwrap_or_else(|| panic!("node {label} should exist"))
}

#[test]
fn retry_timeout_and_compensation_builders_record_sources() {
    let start_line = line!() + 1;
    let axon = Axon::<i32, i32, String>::new("builders")
        .then_with_retry(
            defined_step,
            ranvier_runtime::RetryPolicy::fixed(1, std::time::Duration::from_millis(1)),
        )
        .then_with_timeout(defined_step, std::time::Duration::from_secs(1), || {
            "timeout".to_string()
        })
        .then_compensated(defined_step, compensate_step)
        .compensate_with(compensate_step)
        .branch("left", "Left");
    let nodes = &axon.schematic.nodes;
    // Node order: ingress, retry, timeout, compensated main, its compensation,
    // compensate_with, branch.
    assert_eq!(nodes.len(), 7);
    for (index, offset) in [(1, 2), (2, 6), (3, 9)] {
        assert_location(nodes[index].definition_source.as_ref(), DEFINED_LINE);
        assert_location(nodes[index].attachment_source.as_ref(), start_line + offset);
    }
    // The compensation node that then_compensated adds has no builder call of its own.
    assert_location(nodes[4].definition_source.as_ref(), COMPENSATE_LINE);
    assert!(nodes[4].attachment_source.is_none());
    // compensate_with is an explicit builder call.
    assert_location(nodes[5].definition_source.as_ref(), COMPENSATE_LINE);
    assert_location(nodes[5].attachment_source.as_ref(), start_line + 10);
    // A branch is structural: attachment only.
    assert!(matches!(nodes[6].kind, NodeKind::Synapse));
    assert!(nodes[6].definition_source.is_none());
    assert_location(nodes[6].attachment_source.as_ref(), start_line + 11);
}

#[test]
fn parallel_records_definitions_only_on_branches() {
    let branches: Vec<
        std::sync::Arc<dyn Transition<i32, i32, Resources = (), Error = String> + Send + Sync>,
    > = vec![
        std::sync::Arc::new(defined_step),
        std::sync::Arc::new(ManualStep),
    ];
    let parallel_line = line!() + 2;
    let axon = Axon::<i32, i32, String>::new("parallel")
        .parallel(branches, ranvier_runtime::ParallelStrategy::AllMustSucceed);
    let nodes = &axon.schematic.nodes;
    let fan_out = node_by_label(nodes, "FanOut");
    let fan_in = node_by_label(nodes, "FanIn");
    for structural in [fan_out, fan_in] {
        assert!(structural.definition_source.is_none());
        assert_location(structural.attachment_source.as_ref(), parallel_line);
    }
    let atoms: Vec<_> = nodes
        .iter()
        .filter(|node| matches!(node.kind, NodeKind::Atom))
        .collect();
    assert_eq!(atoms.len(), 2);
    assert_location(atoms[0].definition_source.as_ref(), DEFINED_LINE);
    assert!(atoms[1].definition_source.is_none());
    for atom in atoms {
        assert_location(atom.attachment_source.as_ref(), parallel_line);
    }
}

#[test]
fn traced_wrapper_forwards_the_inner_definition() {
    let traced = ranvier_core::telemetry::Traced::new(defined_step, "traced");
    assert_location(traced.definition_source().as_ref(), DEFINED_LINE);
}

#[cfg(feature = "streaming")]
mod streaming {
    use super::*;
    use ranvier_macros::streaming_transition;

    const STREAM_LINE: u32 = line!() + 2;
    #[streaming_transition]
    async fn numbers(
        input: i32,
    ) -> Result<
        std::pin::Pin<
            Box<dyn ranvier_core::__macro_support::futures_core::Stream<Item = i32> + Send>,
        >,
        String,
    > {
        let _ = input;
        Err("only metadata is inspected".to_string())
    }

    #[test]
    fn then_stream_records_the_user_call_site() {
        let start_line = line!() + 1;
        let streaming = Axon::<i32, i32, String>::new("stream")
            .then_stream(numbers)
            .map_items(|item| item + 1);
        let nodes = &streaming.export_schematic().nodes;
        let stream_node = nodes
            .iter()
            .find(|node| matches!(node.kind, NodeKind::StreamingTransition))
            .expect("streaming node");
        assert_location(stream_node.definition_source.as_ref(), STREAM_LINE);
        assert_location(stream_node.attachment_source.as_ref(), start_line + 2);
        let map_node = node_by_label(nodes, "map_items");
        assert!(map_node.definition_source.is_none());
        assert_location(map_node.attachment_source.as_ref(), start_line + 3);
    }

    #[test]
    fn then_stream_with_timeout_records_the_user_call_site() {
        let start_line = line!() + 1;
        let streaming = Axon::<i32, i32, String>::new("stream-timeout").then_stream_with_timeout(
            numbers,
            ranvier_core::streaming::StreamTimeoutConfig::none(),
        );
        let stream_node = streaming
            .export_schematic()
            .nodes
            .iter()
            .find(|node| matches!(node.kind, NodeKind::StreamingTransition))
            .expect("streaming node");
        assert_location(stream_node.attachment_source.as_ref(), start_line + 1);
    }
}
