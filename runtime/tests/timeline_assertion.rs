use serde::{Deserialize, Serialize};

use ranvier_core::timeline::{Timeline, TimelineEvent};
use ranvier_core::{Bus, Outcome, Transition};
use ranvier_runtime::Axon;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TestInfallible {}

#[derive(Clone)]
struct AddOne;

#[async_trait::async_trait]
impl Transition<u32, u32> for AddOne {
    type Error = TestInfallible;
    type Resources = ();

    async fn run(
        &self,
        state: u32,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<u32, Self::Error> {
        Outcome::next(state + 1)
    }
}

#[tokio::test]
async fn timeline_assertion_works_in_integration_test() {
    let axon = Axon::<u32, u32, TestInfallible, ()>::new("TimelineFlow").then(AddOne);
    let transition_id = axon
        .schematic()
        .nodes
        .last()
        .expect("transition node")
        .id
        .clone();

    let mut bus = Bus::new();
    bus.insert(Timeline::new());

    let outcome = axon.execute(41, &(), &mut bus).await;
    let ranvier_runtime::ExecutionReport::Unpersisted { result: outcome } = outcome else {
        panic!("expected an execution without persistence");
    };
    match outcome {
        Outcome::Next(value) => assert_eq!(value, 42),
        _ => panic!("expected Outcome::Next"),
    }

    let timeline = bus
        .read::<Timeline>()
        .expect("timeline should remain available for assertion");

    let enters: Vec<_> = timeline
        .events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| {
            matches!(event, TimelineEvent::NodeEnter { node_id, .. } if node_id == &transition_id)
                .then_some(index)
        })
        .collect();
    let exits: Vec<_> = timeline
        .events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| {
            matches!(event, TimelineEvent::NodeExit { node_id, .. } if node_id == &transition_id)
                .then_some(index)
        })
        .collect();
    assert_eq!(enters.len(), 1, "one transition must enter exactly once");
    assert_eq!(exits.len(), 1, "one transition must exit exactly once");
    assert!(
        enters[0] < exits[0],
        "transition entry must precede its exit"
    );

    assert!(
        timeline
            .events
            .iter()
            .any(|event| matches!(event, TimelineEvent::NodeEnter { .. })),
        "timeline should include at least one NodeEnter event"
    );
    assert!(
        timeline
            .events
            .iter()
            .any(|event| matches!(event, TimelineEvent::NodeExit { .. })),
        "timeline should include at least one NodeExit event"
    );
}
