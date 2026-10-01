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
struct Step(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl Transition<(), ()> for Step {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: (), _: &(), _: &mut Bus) -> Outcome<(), String> {
        self.0.fetch_add(1, Ordering::SeqCst);
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

#[tokio::test]
async fn readonly_root_saga_context_rejects_before_effect_or_false_cleanup_success() {
    for inherited in [false, true] {
        let mut owner = Bus::new();
        let mut stack = SagaStack::new();
        stack.push("parent-task".into(), "parent".into(), vec![1]);
        owner.insert_shared(stack);
        let mut fork = owner.fork_for_parallel();
        let bus = if inherited { &mut fork } else { &mut owner };
        let ptr = bus.read::<SagaStack>().unwrap() as *const _ as usize;
        let effects = Arc::new(AtomicUsize::new(0));
        let compensated = Arc::new(AtomicUsize::new(0));
        let child = Axon::<(), (), String>::start("readonly-root")
            .with_saga_policy(SagaPolicy::Enabled)
            .then_compensated(Step(effects.clone()), Comp(compensated.clone()))
            .then(Fault(1, Arc::new(tokio::sync::Notify::new())));
        let report = child.execute((), &(), bus).await;
        assert_eq!(
            effects.load(Ordering::SeqCst),
            0,
            "readonly root must reject before business effect"
        );
        assert_eq!(compensated.load(Ordering::SeqCst), 0);
        assert!(
            matches!(report,ExecutionReport::Unpersisted{result:Outcome::Emit(ref event,None),..} if event=="saga.context_readonly")
        );
        assert_eq!(bus.read::<SagaStack>().unwrap().tasks.len(), 1);
        assert_eq!(bus.read::<SagaStack>().unwrap() as *const _ as usize, ptr);
    }
}
