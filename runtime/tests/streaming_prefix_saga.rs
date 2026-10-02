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
/// Initializer cleanup is reported separately from the accepted prefix.
#[tokio::test]
async fn initializer_failure_returns_completed_cleanup_and_drains_root_stack() {
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
        &report,
        StreamInitializationReport::Failed {
            prefix: ExecutionReport::Unpersisted {
                result: (),
                cleanup: None
            },
            initialization: EffectBoundary::MayHaveRun,
            ..
        }
    ));
    let StreamInitializationReport::Failed {
        initialization_cleanup: Some(cleanup),
        ..
    } = &report
    else {
        panic!("initializer cleanup missing");
    };
    assert!(cleanup.is_complete());
    assert_eq!(cleanup.steps.len(), 1);
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(init.load(Ordering::SeqCst), 1);
    assert_eq!(undo.load(Ordering::SeqCst), 1);
    let stack = bus
        .read::<ranvier_core::saga::SagaStack>()
        .expect("root pending work remains in Bus");
    assert!(stack.tasks.is_empty());
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
        let ranvier_runtime::StreamInitializationReport::Failed {
            initialization_cleanup: Some(cleanup),
            ..
        } = &report
        else {
            panic!("child initializer cleanup missing");
        };
        assert!(cleanup.is_complete());
        assert_eq!(cleanup.steps.len(), 1);
        let restored = bus.read::<ranvier_core::saga::SagaStack>().unwrap();
        assert_eq!(restored.tasks.len(), 1);
        assert_eq!(restored.tasks[0].node_id, parent);
        Outcome::Fault("outer-observation".into())
    }
}
#[tokio::test]
async fn nested_initializer_failure_cleans_child_before_parent_restore() {
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
        1,
        "child compensation is independent of parent cleanup"
    );
}

struct Resources(std::sync::Mutex<Vec<(usize, usize)>>);
#[derive(Clone)]
struct ResourceEffect(usize, bool);
#[async_trait::async_trait]
impl Transition<(), ()> for ResourceEffect {
    type Error = String;
    type Resources = Resources;
    async fn run(&self, _: (), res: &Resources, bus: &mut Bus) -> Outcome<(), String> {
        if self.1 {
            assert!(
                bus.cancellation_token().unwrap().context().is_none(),
                "compensation must be shielded"
            );
        }
        res.0
            .lock()
            .unwrap()
            .push((self.0, res as *const Resources as usize));
        Outcome::Next(())
    }
}
#[derive(Clone)]
struct ResourceInit(bool);
#[async_trait::async_trait]
impl StreamingTransition<()> for ResourceInit {
    type Item = ();
    type Error = String;
    type Resources = Resources;
    async fn run_stream(
        &self,
        _: (),
        _: &Resources,
        bus: &mut Bus,
    ) -> Result<Pin<Box<dyn futures_util::Stream<Item = ()> + Send>>, String> {
        if self.0 {
            bus.cancellation_token()
                .unwrap()
                .cancel(ranvier_core::CancellationReason::Explicit);
            std::future::pending().await
        } else {
            Err("initializer-error".into())
        }
    }
}
#[tokio::test]
async fn initializer_failure_and_cancel_cleanup_use_nonclone_resources_lifo_and_shield() {
    use ranvier_runtime::{EffectBoundary, StreamInitializationReport};
    for cancel in [false, true] {
        let res = Resources(std::sync::Mutex::new(Vec::new()));
        let address = &res as *const Resources as usize;
        let caller = ranvier_core::CancellationToken::new();
        let workflow = ranvier_core::CancellationToken::new();
        let mut bus = Bus::new();
        bus.set_cancellation_token(caller.clone());
        let stream = Axon::<(), (), String, Resources>::start("resource-cleanup")
            .with_saga_policy(SagaPolicy::Enabled)
            .then_compensated(ResourceEffect(1, false), ResourceEffect(11, true))
            .then_compensated(ResourceEffect(2, false), ResourceEffect(12, true))
            .then_stream(ResourceInit(cancel));
        let report = stream
            .execute_cancellable((), &res, &mut bus, workflow.clone())
            .await;
        let cleanup = match report {
            StreamInitializationReport::Failed {
                prefix:
                    ExecutionReport::Unpersisted {
                        result: (),
                        cleanup: None,
                    },
                initialization: EffectBoundary::MayHaveRun,
                initialization_cleanup: Some(cleanup),
                ..
            } if !cancel => cleanup,
            StreamInitializationReport::Cancelled {
                prefix:
                    ExecutionReport::Unpersisted {
                        result: (),
                        cleanup: None,
                    },
                initialization: EffectBoundary::MayHaveRun,
                initialization_cleanup: Some(cleanup),
                context,
            } if cancel => {
                assert_eq!(context.reason, ranvier_core::CancellationReason::Explicit);
                cleanup
            }
            _ => panic!("initializer cleanup classification lost"),
        };
        assert!(cleanup.is_complete());
        assert_eq!(cleanup.steps.len(), 2);
        assert_eq!(
            *res.0.lock().unwrap(),
            vec![(1, address), (2, address), (12, address), (11, address)]
        );
        assert_eq!(workflow.context().is_some(), cancel);
        assert!(caller.context().is_none());
        bus.cancellation_token()
            .unwrap()
            .cancel(ranvier_core::CancellationReason::Explicit);
        assert!(
            caller.context().is_some(),
            "caller token allocation must restore"
        );
    }
}

#[tokio::test]
async fn initializer_cleanup_keeps_stored_prefix_confirmation_independent() {
    use ranvier_runtime::{CompletionConfirmation, StreamInitializationReport, persistence::*};
    let effects = Arc::new(AtomicUsize::new(0));
    let undo = Arc::new(AtomicUsize::new(0));
    let init = Arc::new(AtomicUsize::new(0));
    let backing = InMemoryPersistenceStore::new();
    let mut bus = Bus::new();
    bus.insert(PersistenceHandle::from_store(backing.clone()));
    bus.insert(PersistenceTraceId::new("stored-initializer-cleanup"));
    let report = Axon::<(), (), String>::start("stored-initializer-cleanup")
        .with_saga_policy(SagaPolicy::Enabled)
        .then_compensated(Effect(effects.clone()), Effect(undo.clone()))
        .then_stream(InitFail(init.clone()))
        .execute((), &(), &mut bus)
        .await;
    match report {
        StreamInitializationReport::Failed {
            prefix:
                ExecutionReport::Recorded {
                    result: (),
                    completion: CompletionConfirmation::Confirmed,
                    cleanup: None,
                },
            initialization_cleanup: Some(cleanup),
            ..
        } => {
            assert!(cleanup.is_complete());
            assert_eq!(cleanup.steps.len(), 1);
        }
        _ => panic!("prefix acknowledgment and initializer cleanup must stay separate"),
    }
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(undo.load(Ordering::SeqCst), 1);
    assert_eq!(init.load(Ordering::SeqCst), 1);
    assert_eq!(
        backing
            .load("stored-initializer-cleanup")
            .await
            .unwrap()
            .unwrap()
            .completion,
        Some(CompletionState::Success)
    );
}

#[derive(Clone)]
struct GuardedPanic(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl Transition<(), ()> for GuardedPanic {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: (), _: &(), bus: &mut Bus) -> Outcome<(), String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        bus.set_access_policy(
            "guarded-compensator",
            Some(ranvier_core::BusAccessPolicy::deny_only(vec![
                ranvier_core::BusTypeRef::of::<ranvier_runtime::SagaRollbackReport>(),
                ranvier_core::BusTypeRef::of::<ranvier_core::saga::SagaStack>(),
            ])),
        );
        panic!("synthetic compensation panic");
    }
}
#[tokio::test]
async fn panicked_compensator_cannot_hide_initializer_cleanup_or_caller_guard() {
    for panic_last in [true, false] {
        assert_panic_guard(panic_last).await;
    }
}
async fn assert_panic_guard(panic_last: bool) {
    use ranvier_runtime::{SagaRollbackOutcome, StreamInitializationReport};
    let effects = Arc::new(AtomicUsize::new(0));
    let normal_undo = Arc::new(AtomicUsize::new(0));
    let panicked = Arc::new(AtomicUsize::new(0));
    let init = Arc::new(AtomicUsize::new(0));
    let mut bus = Bus::new();
    bus.insert("caller-value".to_string());
    bus.set_access_policy(
        "caller-guard",
        Some(ranvier_core::BusAccessPolicy::deny_only(vec![
            ranvier_core::BusTypeRef::of::<String>(),
        ])),
    );
    let prefix =
        Axon::<(), (), String>::start("guarded-init-cleanup").with_saga_policy(SagaPolicy::Enabled);
    let prefix = if panic_last {
        prefix
            .then_compensated(Effect(effects.clone()), GuardedPanic(panicked.clone()))
            .then_compensated(Effect(effects.clone()), Effect(normal_undo.clone()))
    } else {
        prefix
            .then_compensated(Effect(effects.clone()), Effect(normal_undo.clone()))
            .then_compensated(Effect(effects.clone()), GuardedPanic(panicked.clone()))
    };
    let report = prefix
        .then_stream(InitFail(init.clone()))
        .execute((), &(), &mut bus)
        .await;
    let StreamInitializationReport::Failed {
        prefix:
            ExecutionReport::Unpersisted {
                result: (),
                cleanup: None,
            },
        initialization_cleanup: Some(cleanup),
        ..
    } = report
    else {
        panic!("panic guard hid cleanup report");
    };
    assert!(cleanup.finished);
    assert!(!cleanup.is_complete());
    assert_eq!(
        cleanup
            .steps
            .iter()
            .map(|step| step.outcome)
            .collect::<Vec<_>>(),
        if panic_last {
            vec![
                SagaRollbackOutcome::Completed,
                SagaRollbackOutcome::Panicked,
            ]
        } else {
            vec![
                SagaRollbackOutcome::Panicked,
                SagaRollbackOutcome::Completed,
            ]
        }
    );
    assert_eq!(effects.load(Ordering::SeqCst), 2);
    assert_eq!(normal_undo.load(Ordering::SeqCst), 1);
    assert_eq!(panicked.load(Ordering::SeqCst), 1);
    assert_eq!(init.load(Ordering::SeqCst), 1);
    assert!(
        matches!(bus.get::<String>(), Err(ranvier_core::BusAccessError::Unauthorized { transition, .. }) if transition == "caller-guard")
    );
    assert!(
        bus.read::<ranvier_core::saga::SagaStack>()
            .unwrap()
            .is_empty()
    );
    bus.clear_access_policy();
    assert_eq!(bus.read::<String>().unwrap(), "caller-value");
}

/// Manual trait implementation exercises panic before a future is returned as well
/// as panic during its first poll. Payload is deliberately synthetic, not private.
#[derive(Clone)]
struct PanicInit(bool, Arc<AtomicUsize>);
impl StreamingTransition<()> for PanicInit {
    type Item = ();
    type Error = String;
    type Resources = Resources;
    fn run_stream<'life0, 'life1, 'life2, 'async_trait>(
        &'life0 self,
        _: (),
        _: &'life1 Resources,
        _: &'life2 mut Bus,
    ) -> Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<Pin<Box<dyn futures_util::Stream<Item = ()> + Send>>, String>,
                > + Send
                + 'async_trait,
        >,
    >
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        'life2: 'async_trait,
        Self: 'async_trait,
    {
        self.1.fetch_add(1, Ordering::SeqCst);
        if self.0 {
            panic!("synthetic initializer factory panic");
        }
        Box::pin(async { panic!("synthetic initializer poll panic") })
    }
}
#[tokio::test]
async fn initializer_factory_and_poll_panic_compensate_with_nonclone_resources() {
    use ranvier_runtime::{EffectBoundary, StreamInitializationReport};
    for factory in [false, true] {
        for cancellable in [false, true] {
            let res = Resources(std::sync::Mutex::new(Vec::new()));
            let address = &res as *const Resources as usize;
            let initialized = Arc::new(AtomicUsize::new(0));
            let caller = ranvier_core::CancellationToken::new();
            let mut bus = Bus::new();
            bus.set_cancellation_token(caller.clone());
            let stream = Axon::<(), (), String, Resources>::start("initializer-panic")
                .with_saga_policy(SagaPolicy::Enabled)
                .then_compensated(ResourceEffect(1, false), ResourceEffect(11, true))
                .then_compensated(ResourceEffect(2, false), ResourceEffect(12, true))
                .then_stream(PanicInit(factory, initialized.clone()));
            let report = if cancellable {
                stream
                    .execute_cancellable((), &res, &mut bus, ranvier_core::CancellationToken::new())
                    .await
            } else {
                stream.execute((), &res, &mut bus).await
            };
            let StreamInitializationReport::Failed {
                prefix:
                    ExecutionReport::Unpersisted {
                        result: (),
                        cleanup: None,
                    },
                error,
                initialization: EffectBoundary::MayHaveRun,
                initialization_cleanup: Some(cleanup),
            } = report
            else {
                panic!("panic must retain independent failure/cleanup evidence")
            };
            assert_eq!(error.to_string(), "Stream initialization panicked");
            assert!(cleanup.is_complete());
            assert_eq!(cleanup.steps.len(), 2);
            assert_eq!(initialized.load(Ordering::SeqCst), 1);
            assert_eq!(
                *res.0.lock().unwrap(),
                vec![(1, address), (2, address), (12, address), (11, address)]
            );
            assert!(
                bus.read::<ranvier_core::saga::SagaStack>()
                    .unwrap()
                    .tasks
                    .is_empty()
            );
            bus.cancellation_token()
                .unwrap()
                .cancel(ranvier_core::CancellationReason::Explicit);
            assert!(
                caller.context().is_some(),
                "exact caller token must restore"
            );
        }
    }
}

struct PanicDebug;
impl std::fmt::Debug for PanicDebug {
    fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        panic!("synthetic initializer error formatter panic")
    }
}
#[derive(Clone)]
struct InitDebugFailure;
#[async_trait::async_trait]
impl StreamingTransition<()> for InitDebugFailure {
    type Item = ();
    type Error = PanicDebug;
    type Resources = ();
    async fn run_stream(
        &self,
        _: (),
        _: &(),
        _: &mut Bus,
    ) -> Result<Pin<Box<dyn futures_util::Stream<Item = ()> + Send>>, PanicDebug> {
        Err(PanicDebug)
    }
}
#[tokio::test]
async fn initializer_error_formatter_panic_retains_cleanup() {
    let undo = Arc::new(AtomicUsize::new(0));
    let report = Axon::<(), (), String>::start("initializer-debug-panic")
        .with_saga_policy(SagaPolicy::Enabled)
        .then_compensated(Effect(Arc::new(AtomicUsize::new(0))), Effect(undo.clone()))
        .then_stream(InitDebugFailure)
        .execute((), &(), &mut Bus::new())
        .await;
    let ranvier_runtime::StreamInitializationReport::Failed {
        error,
        initialization_cleanup: Some(cleanup),
        ..
    } = report
    else {
        panic!("formatter panic lost failure evidence")
    };
    assert_eq!(error.to_string(), "Stream initialization panicked");
    assert!(cleanup.is_complete());
    assert_eq!(undo.load(Ordering::SeqCst), 1);
}
