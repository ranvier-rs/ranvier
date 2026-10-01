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
    for cancellable in [false, true] {
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
            let report = if cancellable {
                match child
                    .execute_cancellable((), &(), bus, ranvier_core::CancellationToken::new())
                    .await
                {
                    ExecutionReport::Unpersisted {
                        result: ranvier_runtime::ExecutionTerminal::Outcome(outcome),
                        cleanup,
                    } => ExecutionReport::Unpersisted {
                        result: outcome,
                        cleanup,
                    },
                    _ => panic!("unexpected cancellation or persistence report"),
                }
            } else {
                child.execute((), &(), bus).await
            };
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
}

#[tokio::test]
async fn writable_and_disabled_saga_contexts_keep_declared_execution_semantics() {
    for storage in 0..3 {
        let mut bus = Bus::new();
        if storage == 1 {
            bus.insert(SagaStack::new());
        }
        if storage == 2 {
            bus.insert_shared(SagaStack::new());
        }
        let effects = Arc::new(AtomicUsize::new(0));
        let compensated = Arc::new(AtomicUsize::new(0));
        let circuit = Axon::<(), (), String>::start("owned-root")
            .with_saga_policy(SagaPolicy::Enabled)
            .then_compensated(Step(effects.clone()), Comp(compensated.clone()))
            .then(Fault(1, Arc::new(tokio::sync::Notify::new())));
        let report = circuit.execute((), &(), &mut bus).await;
        assert!(matches!(
            report,
            ExecutionReport::Unpersisted {
                result: Outcome::Fault(_),
                ..
            }
        ));
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        assert_eq!(compensated.load(Ordering::SeqCst), 1);
        let cleanup = report.cleanup().unwrap();
        assert!(cleanup.is_complete());
        assert_eq!(cleanup.steps.len(), 1);
    }
    let mut owner = Bus::new();
    owner.insert_shared(SagaStack::new());
    let mut bus = owner.fork_for_parallel();
    let effects = Arc::new(AtomicUsize::new(0));
    let circuit = Axon::<(), (), String>::start("disabled-root")
        .with_saga_policy(SagaPolicy::Disabled)
        .then(Step(effects.clone()));
    assert!(matches!(
        circuit.execute((), &(), &mut bus).await,
        ExecutionReport::Unpersisted {
            result: Outcome::Next(()),
            ..
        }
    ));
    assert_eq!(effects.load(Ordering::SeqCst), 1);
}
