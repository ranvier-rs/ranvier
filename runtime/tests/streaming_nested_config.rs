#![cfg(feature = "streaming")]
#[cfg(feature = "streaming")]
#[path = "support/stream_reports.rs"]
mod stream_reports;
use ranvier_core::{
    Bus, Outcome, Transition, event::DlqPolicy, saga::SagaPolicy, schematic::Schematic,
    streaming::StreamingTransition,
};
use ranvier_runtime::{Axon, ExecutionReport};
use std::pin::Pin;
#[derive(Clone)]
struct Child(u8, std::sync::Arc<tokio::sync::Notify>);
#[async_trait::async_trait]
impl Transition<(), ()> for Child {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: (), _: &(), _: &mut Bus) -> Outcome<(), String> {
        self.1.notify_one();
        match self.0 {
            0 => Outcome::Next(()),
            1 => Outcome::Fault("child-fault".into()),
            2 => panic!("child-panic"),
            _ => std::future::pending().await,
        }
    }
}
#[derive(Clone)]
struct Parent {
    configured: bool,
    shared: bool,
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
        // A report-aware prefix now installs its actual configuration for the
        // initializer. Establish the intentional absent-parent fixture explicitly
        // before creating the nested child; keep absence restoration coverage.
        assert_eq!(bus.read::<DlqPolicy>(), Some(&DlqPolicy::Drop));
        assert_eq!(bus.read::<SagaPolicy>(), Some(&SagaPolicy::Disabled));
        assert_eq!(bus.read::<Schematic>().unwrap().name, "parent-stream");
        bus.remove::<DlqPolicy>();
        bus.remove::<SagaPolicy>();
        bus.remove::<Schematic>();
        if self.configured {
            if self.shared {
                bus.insert_shared(DlqPolicy::Drop);
                bus.insert_shared(SagaPolicy::Disabled);
                bus.insert_shared(Axon::<(), (), String>::start("parent-config").schematic);
            } else {
                bus.insert(DlqPolicy::Drop);
                bus.insert(SagaPolicy::Disabled);
                bus.insert(Axon::<(), (), String>::start("parent-config").schematic);
            }
        }
        let schematic_ptr = bus.read::<Schematic>().map(|v| v as *const _ as usize);
        let entered = std::sync::Arc::new(tokio::sync::Notify::new());
        let child = Axon::<(), (), String>::start("child-config")
            .with_dlq_policy(DlqPolicy::SendToDlq)
            .with_saga_policy(SagaPolicy::Enabled)
            .then(Child(self.mode, entered.clone()));
        {
            let future = child.execute((), &(), bus);
            tokio::pin!(future);
            if self.mode == 3 {
                tokio::select! {
                    _ = entered.notified() => {},
                    _ = &mut future => panic!("pending child ended"),
                    _ = tokio::time::sleep(std::time::Duration::from_secs(3)) => panic!("child did not enter"),
                }
            } else {
                let report = future.await;
                match report {
                    ExecutionReport::Unpersisted {
                        result: Outcome::Next(()),
                        ..
                    } => assert_eq!(self.mode, 0),
                    ExecutionReport::Unpersisted {
                        result: Outcome::Fault(_),
                        ..
                    } => assert!(self.mode == 1 || self.mode == 2),
                    _ => panic!("unexpected terminal"),
                }
            }
        }
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
        assert_eq!(
            bus.read::<Schematic>().map(|v| v as *const _ as usize),
            schematic_ptr
        );
        assert_eq!(
            bus.fork_for_parallel().read::<Schematic>().is_some(),
            self.configured && self.shared
        );
        assert_eq!(
            bus.fork_for_parallel().read::<DlqPolicy>().is_some(),
            self.configured && self.shared
        );
        assert_eq!(
            bus.fork_for_parallel().read::<SagaPolicy>().is_some(),
            self.configured && self.shared
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
        for shared in [false, true] {
            for mode in 0..4 {
                let circuit = Axon::<(), (), String>::start("parent-stream").then_stream(Parent {
                    configured,
                    shared,
                    mode,
                });
                let mut stream =
                    stream_reports::ready(circuit.execute((), &(), &mut Bus::new()).await);
                assert_eq!(stream.next().await, Some("restored".into()));
            }
        }
    }
}
