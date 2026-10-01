#![cfg(feature = "streaming")]
use ranvier_core::{
    Bus, Outcome, Transition, event::DlqPolicy, saga::SagaPolicy, schematic::Schematic,
    streaming::StreamingTransition,
};
use ranvier_runtime::{Axon, ExecutionReport};
use std::pin::Pin;
#[derive(Clone)]
struct Child(u8);
#[async_trait::async_trait]
impl Transition<(), ()> for Child {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: (), _: &(), _: &mut Bus) -> Outcome<(), String> {
        match self.0 {
            0 => Outcome::Next(()),
            1 => Outcome::Fault("child-fault".into()),
            _ => panic!("child-panic"),
        }
    }
}
#[derive(Clone)]
struct Parent {
    configured: bool,
    mode: u8,
}
#[async_trait::async_trait]
impl StreamingTransition<()> for Parent {
    type Item = String;
    type Error = String;
    type Resources = ();
    async fn run_stream(
        &self,
        _: (),
        _: &(),
        bus: &mut Bus,
    ) -> Result<Pin<Box<dyn futures_util::Stream<Item = String> + Send>>, String> {
        if self.configured {
            bus.insert(DlqPolicy::Drop);
            bus.insert(SagaPolicy::Disabled);
            bus.insert(Axon::<(), (), String>::start("parent-config").schematic);
        }
        let child = Axon::<(), (), String>::start("child-config")
            .with_dlq_policy(DlqPolicy::SendToDlq)
            .with_saga_policy(SagaPolicy::Enabled)
            .then(Child(self.mode));
        let report = child.execute((), &(), bus).await;
        assert!(matches!(report, ExecutionReport::Unpersisted { .. }));
        assert_eq!(
            bus.read::<DlqPolicy>(),
            if self.configured {
                Some(&DlqPolicy::Drop)
            } else {
                None
            }
        );
        assert_eq!(
            bus.read::<SagaPolicy>(),
            if self.configured {
                Some(&SagaPolicy::Disabled)
            } else {
                None
            }
        );
        assert_eq!(
            bus.read::<Schematic>().map(|v| v.name.as_str()),
            if self.configured {
                Some("parent-config")
            } else {
                None
            }
        );
        Ok(Box::pin(futures_util::stream::iter(vec![
            "restored".into(),
        ])))
    }
}
#[tokio::test]
async fn streaming_parent_config_survives_ordinary_child_terminal() {
    use futures_util::StreamExt;
    for configured in [false, true] {
        for mode in 0..3 {
            let circuit = Axon::<(), (), String>::start("parent-stream")
                .then_stream(Parent { configured, mode });
            let mut stream = circuit.execute((), &(), &mut Bus::new()).await.unwrap();
            assert_eq!(stream.next().await, Some("restored".into()));
        }
    }
}
