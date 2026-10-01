#[cfg(feature = "streaming")]
#[path = "support/stream_reports.rs"]
mod stream_reports;
use ranvier_core::{
    Bus, Outcome, Transition,
    saga::{SagaPolicy, SagaStack},
};
use ranvier_runtime::{Axon, ExecutionReport};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
#[derive(Clone)]
struct Step;
#[async_trait::async_trait]
impl Transition<(), ()> for Step {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: (), _: &(), _: &mut Bus) -> Outcome<(), String> {
        Outcome::Next(())
    }
}
#[derive(Clone)]
struct Fault(u8, Arc<tokio::sync::Notify>);
#[async_trait::async_trait]
impl Transition<(), ()> for Fault {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: (), _: &(), _: &mut Bus) -> Outcome<(), String> {
        self.1.notify_one();
        match self.0 {
            0 => Outcome::Next(()),
            1 => Outcome::Fault("child-fault".into()),
            2 => panic!("child-saga-panic"),
            _ => std::future::pending().await,
        }
    }
}
#[derive(Clone)]
struct Comp(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl Transition<(), ()> for Comp {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: (), _: &(), _: &mut Bus) -> Outcome<(), String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::Next(())
    }
}
#[derive(Clone)]
struct Parent {
    storage: u8,
    mode: u8,
}
#[async_trait::async_trait]
impl Transition<(), ()> for Parent {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: (), _: &(), bus: &mut Bus) -> Outcome<(), String> {
        let mut stack = SagaStack::new();
        stack.push("parent-sentinel".into(), "parent-task".into(), vec![1]);
        match self.storage {
            0 => {}
            1 => bus.insert(stack),
            _ => bus.insert_shared(stack),
        }
        let sibling = if self.storage == 3 {
            Some(bus.fork_for_parallel())
        } else {
            None
        };
        let ptr = bus.read::<SagaStack>().map(|v| v as *const _ as usize);
        let entered = Arc::new(tokio::sync::Notify::new());
        let compensated = Arc::new(AtomicUsize::new(0));
        let child = Axon::<(), (), String>::start("child")
            .with_saga_policy(SagaPolicy::Enabled)
            .then_compensated(Step, Comp(compensated.clone()))
            .then(Fault(self.mode, entered.clone()));
        {
            let execution = child.execute((), &(), bus);
            tokio::pin!(execution);
            if self.mode == 3 {
                tokio::select! {
                    _=entered.notified()=>{},
                    _=&mut execution=>panic!("pending child ended"),
                    _=tokio::time::sleep(std::time::Duration::from_secs(3))=>panic!("child did not enter"),
                }
            } else {
                let report = execution.await;
                match report {
                    ExecutionReport::Unpersisted {
                        result: Outcome::Next(()),
                        ..
                    } => assert_eq!(self.mode, 0),
                    ExecutionReport::Unpersisted {
                        result: Outcome::Fault(_),
                        ..
                    } => {
                        assert!(self.mode == 1 || self.mode == 2);
                        assert!(report.cleanup().is_some());
                    }
                    _ => panic!("unexpected child report"),
                }
            }
        }
        assert_eq!(
            compensated.load(Ordering::SeqCst),
            usize::from(self.mode == 1 || self.mode == 2)
        );
        assert_eq!(bus.read::<SagaStack>().map(|v| v as *const _ as usize), ptr);
        if self.storage == 0 {
            assert!(bus.read::<SagaStack>().is_none());
        } else {
            let stack = bus.read::<SagaStack>().unwrap();
            assert_eq!(stack.tasks.len(), 1, "child changed parent tasks");
            assert_eq!(stack.tasks[0].node_id, "parent-sentinel");
            assert_eq!(stack.tasks[0].input_snapshot, vec![1]);
        }
        assert_eq!(
            bus.fork_for_parallel().read::<SagaStack>().is_some(),
            self.storage >= 2
        );
        if let Some(sibling) = sibling {
            assert_eq!(sibling.read::<SagaStack>().unwrap().tasks.len(), 1);
        }
        Outcome::Next(())
    }
}

#[tokio::test]
async fn child_saga_rollback_does_not_consume_parent_tasks() {
    for storage in 0..4 {
        for mode in 0..4 {
            let parent = Axon::<(), (), String>::start("parent").then(Parent { storage, mode });
            assert!(matches!(
                parent.execute((), &(), &mut Bus::new()).await,
                ExecutionReport::Unpersisted {
                    result: Outcome::Next(()),
                    ..
                }
            ));
        }
    }
}
#[cfg(feature = "streaming")]
#[async_trait::async_trait]
impl ranvier_core::streaming::StreamingTransition<()> for Parent {
    type Item = String;
    type Error = String;
    type Resources = ();
    async fn run_stream(
        &self,
        _: (),
        res: &(),
        bus: &mut Bus,
    ) -> Result<std::pin::Pin<Box<dyn futures_util::Stream<Item = String> + Send>>, String> {
        assert!(matches!(self.run((), res, bus).await, Outcome::Next(())));
        Ok(Box::pin(futures_util::stream::iter(vec![
            "parent-intact".into(),
        ])))
    }
}
#[cfg(feature = "streaming")]
#[tokio::test]
async fn streaming_parent_keeps_its_tasks_after_ordinary_saga_child() {
    use futures_util::StreamExt;
    for storage in 0..4 {
        for mode in 0..4 {
            let parent = Axon::<(), (), String>::start("stream-parent")
                .then_stream(Parent { storage, mode });
            let mut stream = stream_reports::ready(parent.execute((), &(), &mut Bus::new()).await);
            assert_eq!(stream.next().await, Some("parent-intact".into()));
        }
    }
}
