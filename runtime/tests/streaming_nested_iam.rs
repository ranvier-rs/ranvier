#![cfg(feature = "streaming")]
use ranvier_core::{
    Bus, Outcome, Transition,
    iam::{IamIdentity, IamToken},
    streaming::StreamingTransition,
};
use ranvier_runtime::{Axon, ExecutionReport};
use std::{pin::Pin, sync::Arc};
#[derive(Clone)]
struct Mutate {
    mode: u8,
    entered: Arc<tokio::sync::Notify>,
}
#[async_trait::async_trait]
impl StreamingTransition<()> for Mutate {
    type Item = String;
    type Error = String;
    type Resources = ();
    async fn run_stream(
        &self,
        _: (),
        _: &(),
        bus: &mut Bus,
    ) -> Result<Pin<Box<dyn futures_util::Stream<Item = String> + Send>>, String> {
        bus.remove::<IamIdentity>();
        bus.remove::<IamToken>();
        bus.insert_shared(IamIdentity::new("child"));
        bus.insert_shared(IamToken("child-token".into()));
        self.entered.notify_one();
        match self.mode {
            0 => Ok(Box::pin(futures_util::stream::iter(vec![
                "child-item".into(),
            ]))),
            1 => Err("synthetic child init error".into()),
            2 => panic!("synthetic child init panic"),
            _ => std::future::pending().await,
        }
    }
}
#[derive(Clone)]
struct Parent {
    present: bool,
    shared: bool,
    mode: u8,
}
#[async_trait::async_trait]
impl Transition<(), ()> for Parent {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: (), _: &(), bus: &mut Bus) -> Outcome<(), String> {
        use futures_util::FutureExt;
        if self.present {
            let identity = IamIdentity::new("parent").with_role("parent-role");
            let token = IamToken("parent-token".into());
            if self.shared {
                bus.insert_shared(identity);
                bus.insert_shared(token);
            } else {
                bus.insert(identity);
                bus.insert(token);
            }
        }
        let identity_ptr = bus.read::<IamIdentity>().map(|v| v as *const _ as usize);
        let token_ptr = bus.read::<IamToken>().map(|v| v as *const _ as usize);
        let entered = Arc::new(tokio::sync::Notify::new());
        let child = Axon::<(), (), String>::start("nested-stream").then_stream(Mutate {
            mode: self.mode,
            entered: entered.clone(),
        });
        {
            let future = std::panic::AssertUnwindSafe(child.execute((), &(), bus)).catch_unwind();
            tokio::pin!(future);
            if self.mode == 3 {
                tokio::select! {
                    _ = entered.notified() => {},
                    _ = &mut future => panic!("pending child unexpectedly ended"),
                    _ = tokio::time::sleep(std::time::Duration::from_secs(3)) => panic!("child did not enter"),
                }
            } else {
                let result = future.await;
                assert_eq!(result.is_err(), self.mode == 2);
                if let Ok(result) = result {
                    assert_eq!(result.is_err(), self.mode == 1);
                }
            }
        }
        assert_eq!(
            bus.read::<IamIdentity>().map(|v| v as *const _ as usize),
            identity_ptr,
            "parent identity storage changed"
        );
        assert_eq!(
            bus.read::<IamToken>().map(|v| v as *const _ as usize),
            token_ptr,
            "parent token storage changed"
        );
        assert_eq!(
            bus.read::<IamIdentity>().map(|v| v.subject.as_str()),
            if self.present { Some("parent") } else { None }
        );
        assert_eq!(
            bus.read::<IamToken>().map(|v| v.0.as_str()),
            if self.present {
                Some("parent-token")
            } else {
                None
            }
        );
        assert_eq!(
            bus.fork_for_parallel().read::<IamIdentity>().is_some(),
            self.present && self.shared
        );
        assert_eq!(
            bus.fork_for_parallel().read::<IamToken>().is_some(),
            self.present && self.shared
        );
        Outcome::Next(())
    }
}
#[tokio::test]
async fn ordinary_parent_restores_exact_iam_after_stream_return_unwind_and_drop() {
    for present in [false, true] {
        for shared in [false, true] {
            for mode in 0..4 {
                let parent = Axon::<(), (), String>::start("ordinary-parent").then(Parent {
                    present,
                    shared,
                    mode,
                });
                let report = parent.execute((), &(), &mut Bus::new()).await;
                assert!(matches!(
                    report,
                    ExecutionReport::Unpersisted {
                        result: Outcome::Next(()),
                        ..
                    }
                ));
            }
        }
    }
}
