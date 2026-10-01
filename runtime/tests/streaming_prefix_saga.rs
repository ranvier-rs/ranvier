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
        matches!(result, ranvier_runtime::StreamInitializationReport::Rejected {
        prefix: ExecutionReport::Unpersisted {
            result: ranvier_runtime::ExecutionTerminal::Outcome(Outcome::Fault(ref error)), ..
        }
    } if error == "prefix-fault")
    );
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(initialized.load(Ordering::SeqCst), 0);
    assert_eq!(
        compensations.load(Ordering::SeqCst),
        1,
        "converted prefix must compensate the already completed effect"
    );
}
