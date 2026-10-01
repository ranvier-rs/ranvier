#![cfg(feature = "streaming")]
use ranvier_core::{
    Bus, event::DlqPolicy, saga::SagaPolicy, schematic::Schematic, streaming::StreamingTransition,
};
use ranvier_runtime::Axon;
use std::pin::Pin;
#[derive(Clone)]
struct Child(u8, std::sync::Arc<tokio::sync::Notify>);
#[async_trait::async_trait]
impl StreamingTransition<()> for Child {
    type Item = String;
    type Error = String;
    type Resources = ();
    async fn run_stream(
        &self,
        _: (),
        _: &(),
        bus: &mut Bus,
    ) -> Result<Pin<Box<dyn futures_util::Stream<Item = String> + Send>>, String> {
        bus.insert(DlqPolicy::SendToDlq);
        bus.insert(SagaPolicy::Enabled);
        bus.insert(Axon::<(), (), String>::start("child-config").schematic);
        self.1.notify_one();
        match self.0 {
            0 => Ok(Box::pin(futures_util::stream::iter(vec![
                "child-item".into(),
            ]))),
            1 => Err("child-error".into()),
            2 => panic!("child-stream-panic"),
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
            .then_stream(Child(self.mode, entered.clone()));
        {
            use futures_util::FutureExt;
            let future = std::panic::AssertUnwindSafe(child.execute((), &(), bus)).catch_unwind();
            tokio::pin!(future);
            if self.mode == 3 {
                tokio::select! {
                    _ = entered.notified() => {},
                    _ = &mut future => panic!("pending child ended"),
                    _ = tokio::time::sleep(std::time::Duration::from_secs(3)) => panic!("child did not enter"),
                }
            } else {
                let result = future.await;
                assert_eq!(result.is_err(), self.mode == 2);
                if let Ok(initialized) = result {
                    assert_eq!(initialized.is_err(), self.mode == 1);
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
async fn streaming_parent_config_survives_stream_child_return_unwind_and_drop() {
    use futures_util::StreamExt;
    for configured in [false, true] {
        for shared in [false, true] {
            for mode in 0..4 {
                let circuit = Axon::<(), (), String>::start("parent-stream").then_stream(Parent {
                    configured,
                    shared,
                    mode,
                });
                let mut stream = circuit.execute((), &(), &mut Bus::new()).await.unwrap();
                assert_eq!(stream.next().await, Some("restored".into()));
            }
        }
    }
}
