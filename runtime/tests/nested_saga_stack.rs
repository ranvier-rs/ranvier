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
struct Fault;
#[async_trait::async_trait]
impl Transition<(), ()> for Fault {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: (), _: &(), _: &mut Bus) -> Outcome<(), String> {
        Outcome::Fault("child-fault".into())
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
struct Parent;
#[async_trait::async_trait]
impl Transition<(), ()> for Parent {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: (), _: &(), bus: &mut Bus) -> Outcome<(), String> {
        let mut stack = SagaStack::new();
        stack.push("parent-sentinel".into(), "parent-task".into(), vec![1]);
        bus.insert(stack);
        let ptr = bus.read::<SagaStack>().unwrap() as *const _ as usize;
        let compensated = Arc::new(AtomicUsize::new(0));
        let child = Axon::<(), (), String>::start("child")
            .with_saga_policy(SagaPolicy::Enabled)
            .then_compensated(Step, Comp(compensated.clone()))
            .then(Fault);
        let report = child.execute((), &(), bus).await;
        assert!(matches!(
            report,
            ExecutionReport::Unpersisted {
                result: Outcome::Fault(_),
                ..
            }
        ));
        assert_eq!(compensated.load(Ordering::SeqCst), 1);
        let stack = bus.read::<SagaStack>().unwrap();
        assert_eq!(
            stack.tasks.len(),
            1,
            "child rollback consumed parent Saga stack"
        );
        assert_eq!(stack.tasks[0].node_id, "parent-sentinel");
        assert_eq!(stack as *const _ as usize, ptr);
        Outcome::Next(())
    }
}
#[tokio::test]
async fn child_saga_rollback_does_not_consume_parent_tasks() {
    let parent = Axon::<(), (), String>::start("parent").then(Parent);
    assert!(matches!(
        parent.execute((), &(), &mut Bus::new()).await,
        ExecutionReport::Unpersisted {
            result: Outcome::Next(()),
            ..
        }
    ));
}
