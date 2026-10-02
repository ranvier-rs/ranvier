#![cfg(feature = "streaming")]
use ranvier_core::{Bus, Outcome, Transition, saga::SagaPolicy, streaming::StreamingTransition};
use ranvier_runtime::{Axon, ExecutionReport};
use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

#[derive(Clone)]
struct Effect(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl Transition<(), ()> for Effect {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: (), _: &(), _: &mut Bus) -> Outcome<(), String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::Next(())
    }
}
#[derive(Clone)]
struct Fail;
#[async_trait::async_trait]
impl Transition<(), ()> for Fail {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: (), _: &(), _: &mut Bus) -> Outcome<(), String> {
        Outcome::Fault("prefix-fault".into())
    }
}
#[derive(Clone)]
struct Init(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl StreamingTransition<()> for Init {
    type Item = ();
    type Error = String;
    type Resources = ();
    async fn run_stream(
        &self,
        _: (),
        _: &(),
        _: &mut Bus,
    ) -> Result<Pin<Box<dyn futures_util::Stream<Item = ()> + Send>>, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Box::pin(futures_util::stream::empty()))
    }
}

#[tokio::test]
async fn configured_saga_survives_then_stream_conversion() {
    let effects = Arc::new(AtomicUsize::new(0));
    let compensations = Arc::new(AtomicUsize::new(0));
    let initialized = Arc::new(AtomicUsize::new(0));
    let prefix = Axon::<(), (), String>::start("configured-saga-prefix")
        .with_saga_policy(SagaPolicy::Enabled)
        .then_compensated(Effect(effects.clone()), Effect(compensations.clone()))
        .then(Fail);
    let report = prefix.clone().execute((), &(), &mut Bus::new()).await;
    assert!(matches!(
        report,
        ExecutionReport::Unpersisted {
            result: Outcome::Fault(_),
            ..
        }
    ));
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(compensations.load(Ordering::SeqCst), 1);
    assert!(report.cleanup().is_some());
    effects.store(0, Ordering::SeqCst);
    compensations.store(0, Ordering::SeqCst);
    let result = prefix
        .then_stream(Init(initialized.clone()))
        .execute((), &(), &mut Bus::new())
        .await;
    assert!(
        matches!(&result, ranvier_runtime::StreamInitializationReport::Rejected {
        prefix: ExecutionReport::Unpersisted {
            result: ranvier_runtime::ExecutionTerminal::Outcome(Outcome::Fault(error)), ..
        }
    } if error == "prefix-fault")
    );
    let ranvier_runtime::StreamInitializationReport::Rejected { prefix } = &result else {
        panic!("expected rejected prefix report");
    };
    let cleanup = prefix
        .cleanup()
        .expect("converted prefix must retain cleanup evidence");
    assert!(cleanup.is_complete());
    assert_eq!(cleanup.steps.len(), 1);
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(initialized.load(Ordering::SeqCst), 0);
    assert_eq!(
        compensations.load(Ordering::SeqCst),
        1,
        "converted prefix must compensate the already completed effect"
    );
}

#[derive(Clone)]
struct InitFail(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl StreamingTransition<()> for InitFail {
    type Item = ();
    type Error = String;
    type Resources = ();
    async fn run_stream(
        &self,
        _: (),
        _: &(),
        _: &mut Bus,
    ) -> Result<Pin<Box<dyn futures_util::Stream<Item = ()> + Send>>, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err("initializer-fault".into())
    }
}
/// Characterization of OPEN ownership loss, not an acceptance assertion that
/// unperformed cleanup is safe. Replace with desired cleanup/recovery assertions
/// when the owned initializer/consumption handoff is implemented.
#[tokio::test]
async fn initializer_failure_retains_root_stack_without_reporting_cleanup() {
    use ranvier_runtime::{EffectBoundary, StreamInitializationReport};
    let effects = Arc::new(AtomicUsize::new(0));
    let undo = Arc::new(AtomicUsize::new(0));
    let init = Arc::new(AtomicUsize::new(0));
    let stream = Axon::<(), (), String>::start("initializer-saga-ownership")
        .with_saga_policy(SagaPolicy::Enabled)
        .then_compensated(Effect(effects.clone()), Effect(undo.clone()))
        .then_stream(InitFail(init.clone()));
    let mut bus = Bus::new();
    let report = stream.execute((), &(), &mut bus).await;
    assert!(matches!(
        report,
        StreamInitializationReport::Failed {
            prefix: ExecutionReport::Unpersisted {
                result: (),
                cleanup: None
            },
            initialization: EffectBoundary::MayHaveRun,
            ..
        }
    ));
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(init.load(Ordering::SeqCst), 1);
    assert_eq!(
        undo.load(Ordering::SeqCst),
        0,
        "this exposes unperformed undo, not safety"
    );
    let stack = bus
        .read::<ranvier_core::saga::SagaStack>()
        .expect("root pending work remains in Bus");
    assert_eq!(stack.tasks.len(), 1);
    assert_eq!(stack.tasks[0].input_snapshot, b"null");
}

#[derive(Clone)]
struct NestedInitFailure(Arc<AtomicUsize>, Arc<AtomicUsize>, Arc<AtomicUsize>);
#[async_trait::async_trait]
impl Transition<(), ()> for NestedInitFailure {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: (), _: &(), bus: &mut Bus) -> Outcome<(), String> {
        let parent = bus.read::<ranvier_core::saga::SagaStack>().unwrap().tasks[0]
            .node_id
            .clone();
        let report = Axon::<(), (), String>::start("nested-init-failure")
            .with_saga_policy(SagaPolicy::Enabled)
            .then_compensated(Effect(self.0.clone()), Effect(self.1.clone()))
            .then_stream(InitFail(self.2.clone()))
            .execute((), &(), bus)
            .await;
        assert!(matches!(
            report,
            ranvier_runtime::StreamInitializationReport::Failed {
                prefix: ExecutionReport::Unpersisted {
                    result: (),
                    cleanup: None
                },
                ..
            }
        ));
        let restored = bus.read::<ranvier_core::saga::SagaStack>().unwrap();
        assert_eq!(restored.tasks.len(), 1);
        assert_eq!(restored.tasks[0].node_id, parent);
        Outcome::Fault("outer-observation".into())
    }
}
#[tokio::test]
async fn nested_initializer_failure_restores_parent_but_exposes_unowned_child_cleanup() {
    let parent = Arc::new(AtomicUsize::new(0));
    let parent_undo = Arc::new(AtomicUsize::new(0));
    let child = Arc::new(AtomicUsize::new(0));
    let child_undo = Arc::new(AtomicUsize::new(0));
    let init = Arc::new(AtomicUsize::new(0));
    let report = Axon::<(), (), String>::start("parent-init-observation")
        .with_saga_policy(SagaPolicy::Enabled)
        .then_compensated(Effect(parent.clone()), Effect(parent_undo.clone()))
        .then(NestedInitFailure(
            child.clone(),
            child_undo.clone(),
            init.clone(),
        ))
        .execute((), &(), &mut Bus::new())
        .await;
    assert!(matches!(
        report,
        ExecutionReport::Unpersisted {
            result: Outcome::Fault(_),
            ..
        }
    ));
    assert!(report.cleanup().unwrap().is_complete());
    assert_eq!(parent.load(Ordering::SeqCst), 1);
    assert_eq!(parent_undo.load(Ordering::SeqCst), 1);
    assert_eq!(child.load(Ordering::SeqCst), 1);
    assert_eq!(init.load(Ordering::SeqCst), 1);
    assert_eq!(
        child_undo.load(Ordering::SeqCst),
        0,
        "OPEN: parent cleanup cannot substitute for missing child cleanup"
    );
}
