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

    let mut bus = Bus::new();
    bus.insert(Timeline::new());

    let outcome = axon.execute(41, &(), &mut bus).await;
    match outcome {
        Outcome::Next(value) => assert_eq!(value, 42),
        _ => panic!("expected Outcome::Next"),
    }

    let timeline = bus
        .read::<Timeline>()
        .expect("timeline should remain available for assertion");

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
