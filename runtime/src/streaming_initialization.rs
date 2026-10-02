//! Typed prefix-to-stream handoff. Prefix evidence does not certify late stream completion.
use crate::{EffectBoundary, ExecutionReport, ExecutionTerminal};
use ranvier_core::{CancellationContext, Outcome};

/// Owned initializer error detail for application diagnostics. Adapters must use
/// a fixed public classification rather than exposing this potentially private text.
#[derive(Debug)]
pub enum StreamInitializationError {
    Transition(String),
    /// The initializer unwound. No panic payload is retained; process hooks may log it.
    Panicked,
}
impl std::fmt::Display for StreamInitializationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transition(detail) => write!(f, "Stream initialization failed: {detail}"),
            Self::Panicked => f.write_str("Stream initialization panicked"),
        }
    }
}
impl std::error::Error for StreamInitializationError {}

/// Initialization evidence, retaining rejected prefix results without serialization or cloning.
/// `Value` can be an owned stream or a collected value; it need not implement Serialize.
#[must_use = "inspect prefix durability and initialization state before acknowledging success"]
pub enum StreamInitializationReport<Out, Value, E> {
    Ready {
        prefix: ExecutionReport<()>,
        value: Value,
    },
    Rejected {
        prefix: ExecutionReport<ExecutionTerminal<Out, E>>,
    },
    Failed {
        prefix: ExecutionReport<()>,
        error: StreamInitializationError,
        initialization: EffectBoundary,
        initialization_cleanup: Option<crate::SagaRollbackReport>,
    },
    Cancelled {
        prefix: ExecutionReport<()>,
        context: CancellationContext,
        initialization: EffectBoundary,
        initialization_cleanup: Option<crate::SagaRollbackReport>,
    },
}

impl<Out, Value, E> StreamInitializationReport<Out, Value, E> {
    /// Transform an initialized value only, retaining every rejection and observation.
    /// This function does not turn stream collection into a durable workflow completion.
    pub fn map_value<New>(
        self,
        map: impl FnOnce(Value) -> New,
    ) -> StreamInitializationReport<Out, New, E> {
        match self {
            Self::Ready { prefix, value } => StreamInitializationReport::Ready {
                prefix,
                value: map(value),
            },
            Self::Rejected { prefix } => StreamInitializationReport::Rejected { prefix },
            Self::Failed {
                prefix,
                error,
                initialization,
                initialization_cleanup,
            } => StreamInitializationReport::Failed {
                prefix,
                error,
                initialization,
                initialization_cleanup,
            },
            Self::Cancelled {
                prefix,
                context,
                initialization,
                initialization_cleanup,
            } => StreamInitializationReport::Cancelled {
                prefix,
                context,
                initialization,
                initialization_cleanup,
            },
        }
    }
}

/// Entry is allowed only after an observed Next without uncertainty or incomplete cleanup.
/// Recorded/NotRequested retain application-owned completion semantics, as ordinary Axon
/// consumers do. They never imply a completion write that was not acknowledged.
pub(crate) fn admit_prefix<Out, E>(
    report: ExecutionReport<ExecutionTerminal<Out, E>>,
) -> Result<(Out, ExecutionReport<()>), ExecutionReport<ExecutionTerminal<Out, E>>> {
    let cleanup_complete = report.cleanup().is_none_or(|cleanup| cleanup.is_complete());
    if !cleanup_complete {
        return Err(report);
    }
    match report {
        ExecutionReport::Unpersisted {
            result: ExecutionTerminal::Outcome(Outcome::Next(output)),
            cleanup,
        } => Ok((
            output,
            ExecutionReport::Unpersisted {
                result: (),
                cleanup,
            },
        )),
        ExecutionReport::NotRequested {
            result: ExecutionTerminal::Outcome(Outcome::Next(output)),
            cleanup,
        } => Ok((
            output,
            ExecutionReport::NotRequested {
                result: (),
                cleanup,
            },
        )),
        ExecutionReport::Recorded {
            result: ExecutionTerminal::Outcome(Outcome::Next(output)),
            completion,
            cleanup,
        } => Ok((
            output,
            ExecutionReport::Recorded {
                result: (),
                completion,
                cleanup,
            },
        )),
        rejected => Err(rejected),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Axon, CompletionConfirmation, PersistenceOperation};
    use ranvier_core::{Bus, Transition, saga::SagaPolicy, streaming::StreamingTransition};
    use std::{
        pin::Pin,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    #[test]
    fn admission_retains_unknown_results_and_cleanup_without_clone() {
        struct Owned(Box<u32>);
        for effect in [EffectBoundary::NotInvoked, EffectBoundary::MayHaveRun] {
            let pointer = Box::new(42);
            let address = &*pointer as *const u32;
            let report = ExecutionReport::Unconfirmed {
                result: ExecutionTerminal::<Owned, String>::Outcome(Outcome::Next(Owned(pointer))),
                operation: PersistenceOperation::Complete,
                effect,
                cleanup: None,
            };
            let rejected = match admit_prefix(report) {
                Err(report) => report,
                Ok(_) => panic!("unknown admitted"),
            };
            match rejected {
                ExecutionReport::Unconfirmed {
                    result: ExecutionTerminal::Outcome(Outcome::Next(value)),
                    operation,
                    effect: actual,
                    ..
                } => {
                    assert_eq!(&*value.0 as *const u32, address);
                    assert_eq!(operation, PersistenceOperation::Complete);
                    assert_eq!(actual, effect);
                }
                _ => panic!("known result lost"),
            }
        }
        for completion in [
            CompletionConfirmation::Confirmed,
            CompletionConfirmation::NotRequested,
        ] {
            let report = ExecutionReport::<ExecutionTerminal<u32, String>>::Recorded {
                result: ExecutionTerminal::Outcome(Outcome::Next(42)),
                completion,
                cleanup: None,
            };
            match admit_prefix(report) {
                Ok((
                    42,
                    ExecutionReport::Recorded {
                        completion: actual,
                        result: (),
                        cleanup: None,
                    },
                )) => assert_eq!(actual, completion),
                _ => panic!("completion policy changed"),
            }
        }
        let report = ExecutionReport::<ExecutionTerminal<u32, String>>::Unpersisted {
            result: ExecutionTerminal::Outcome(Outcome::Next(42)),
            cleanup: Some(crate::SagaRollbackReport {
                trace_id: "partial".into(),
                finished: false,
                steps: vec![],
            }),
        };
        assert!(admit_prefix(report).is_err());
        let blocked = ExecutionReport::<ExecutionTerminal<Owned, String>>::BlockedBeforeEffect {
            operation: PersistenceOperation::Intent,
            effect: EffectBoundary::MayHaveRun,
            cleanup: None,
        };
        assert!(matches!(
            admit_prefix(blocked),
            Err(ExecutionReport::BlockedBeforeEffect {
                effect: EffectBoundary::MayHaveRun,
                ..
            })
        ));
    }

    #[derive(Clone)]
    struct Effect(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl Transition<usize, usize> for Effect {
        type Error = String;
        type Resources = ();
        async fn run(&self, input: usize, _: &(), _: &mut Bus) -> Outcome<usize, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Outcome::Next(input)
        }
    }
    #[derive(Clone)]
    struct Comp(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl Transition<usize, ()> for Comp {
        type Error = String;
        type Resources = ();
        async fn run(&self, _: usize, _: &(), _: &mut Bus) -> Outcome<(), String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Outcome::Next(())
        }
    }
    #[derive(Clone)]
    struct Finish {
        pending: Option<Arc<tokio::sync::Notify>>,
    }
    #[async_trait::async_trait]
    impl Transition<usize, usize> for Finish {
        type Error = String;
        type Resources = ();
        async fn run(&self, _: usize, _: &(), _: &mut Bus) -> Outcome<usize, String> {
            if let Some(entered) = &self.pending {
                entered.notify_one();
                std::future::pending().await
            } else {
                Outcome::Fault("prefix-fault".into())
            }
        }
    }
    struct Init(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl StreamingTransition<usize> for Init {
        type Item = usize;
        type Error = String;
        type Resources = ();
        async fn run_stream(
            &self,
            input: usize,
            _: &(),
            _: &mut Bus,
        ) -> Result<Pin<Box<dyn futures_core::Stream<Item = usize> + Send>>, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Box::pin(futures_util::stream::iter(vec![input])))
        }
    }

    #[tokio::test]
    async fn whole_prefix_compensates_fault_and_cancellation_before_initialization() {
        for cancelled in [false, true] {
            let effects = Arc::new(AtomicUsize::new(0));
            let compensations = Arc::new(AtomicUsize::new(0));
            let initialized = Arc::new(AtomicUsize::new(0));
            let entered = Arc::new(tokio::sync::Notify::new());
            let prefix = Axon::<usize, usize, String>::start("whole-prefix")
                .with_saga_policy(SagaPolicy::Enabled)
                .then_compensated(Effect(effects.clone()), Comp(compensations.clone()))
                .then(Finish {
                    pending: cancelled.then(|| entered.clone()),
                });
            let token = ranvier_core::CancellationToken::new();
            let mut bus = Bus::new();
            let init = Init(initialized.clone());
            let execution = prefix.initialize_stream(
                42,
                &(),
                &mut bus,
                &init,
                cancelled.then(|| token.clone()),
            );
            let report = if cancelled {
                let cancel = async {
                    tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
                        .await
                        .unwrap();
                    token.cancel(ranvier_core::CancellationReason::Explicit);
                };
                tokio::join!(execution, cancel).0
            } else {
                execution.await
            };
            match report {
                StreamInitializationReport::Rejected { prefix } => {
                    assert!(prefix.cleanup().unwrap().is_complete());
                    match prefix {
                        ExecutionReport::Unpersisted {
                            result: ExecutionTerminal::Cancelled(_),
                            ..
                        } => assert!(cancelled),
                        ExecutionReport::Unpersisted {
                            result: ExecutionTerminal::Outcome(Outcome::Fault(error)),
                            ..
                        } => {
                            assert!(!cancelled);
                            assert_eq!(error, "prefix-fault");
                        }
                        _ => panic!("unexpected prefix report"),
                    }
                }
                _ => panic!("failed prefix initialized stream"),
            }
            assert_eq!(effects.load(Ordering::SeqCst), 1);
            assert_eq!(compensations.load(Ordering::SeqCst), 1);
            assert_eq!(initialized.load(Ordering::SeqCst), 0);
        }
    }

    struct Verifier(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl ranvier_core::iam::IamVerifier for Verifier {
        async fn verify(
            &self,
            _: &str,
        ) -> Result<ranvier_core::iam::IamIdentity, ranvier_core::iam::IamError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ranvier_core::iam::IamIdentity::new("verified-child"))
        }
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    struct MoveOnly(Vec<usize>);
    #[derive(Clone)]
    struct Produce;
    #[async_trait::async_trait]
    impl Transition<usize, MoveOnly> for Produce {
        type Error = String;
        type Resources = ();
        async fn run(&self, input: usize, _: &(), _: &mut Bus) -> Outcome<MoveOnly, String> {
            Outcome::Next(MoveOnly(vec![input]))
        }
    }
    struct VerifyInit;
    #[async_trait::async_trait]
    impl StreamingTransition<MoveOnly> for VerifyInit {
        type Item = usize;
        type Error = String;
        type Resources = ();
        async fn run_stream(
            &self,
            input: MoveOnly,
            _: &(),
            bus: &mut Bus,
        ) -> Result<Pin<Box<dyn futures_core::Stream<Item = usize> + Send>>, String> {
            assert_eq!(
                bus.read::<ranvier_core::iam::IamIdentity>()
                    .unwrap()
                    .subject,
                "verified-child"
            );
            assert_eq!(
                bus.read::<ranvier_core::schematic::Schematic>()
                    .unwrap()
                    .name,
                "typed-handoff"
            );
            Ok(Box::pin(futures_util::stream::iter(input.0)))
        }
    }
    #[tokio::test]
    async fn nested_initializer_sees_verified_context_before_exact_parent_restore() {
        use futures_util::StreamExt;
        use ranvier_core::iam::{IamIdentity, IamPolicy, IamToken};
        let verified = Arc::new(AtomicUsize::new(0));
        let prefix = Axon::<usize, usize, String>::start("typed-handoff")
            .with_iam(IamPolicy::RequireIdentity, Verifier(verified.clone()))
            .then(Produce);
        let mut bus = Bus::new();
        let mut parent = crate::execution_observation::InvocationOwner::begin(&mut bus);
        parent.bus().insert_shared(IamIdentity::new("parent"));
        parent.bus().insert(IamToken("fixture".into()));
        let pointer = parent.bus().read::<IamIdentity>().unwrap() as *const _;
        let report = prefix
            .initialize_stream(42, &(), parent.bus(), &VerifyInit, None)
            .await;
        let StreamInitializationReport::Ready {
            prefix:
                ExecutionReport::Unpersisted {
                    result: (),
                    cleanup: None,
                },
            value,
        } = report
        else {
            panic!("handoff was not ready")
        };
        assert_eq!(value.collect::<Vec<_>>().await, vec![42]);
        assert_eq!(verified.load(Ordering::SeqCst), 1);
        assert_eq!(
            parent.bus().read::<IamIdentity>().unwrap() as *const _,
            pointer
        );
        assert_eq!(
            parent.bus().read::<IamIdentity>().unwrap().subject,
            "parent"
        );
    }
    struct AckStore {
        backing: crate::InMemoryPersistenceStore,
        mode: u8,
    }
    #[async_trait::async_trait]
    impl crate::PersistenceStore for AckStore {
        async fn append(&self, event: crate::PersistenceEnvelope) -> anyhow::Result<()> {
            self.backing.append(event).await?;
            if self.mode == 1 {
                anyhow::bail!("synthetic unknown entry ACK");
            }
            Ok(())
        }
        async fn load(&self, trace: &str) -> anyhow::Result<Option<crate::PersistedTrace>> {
            if self.mode == 3 {
                anyhow::bail!("synthetic unavailable history");
            }
            self.backing.load(trace).await
        }
        async fn resume(&self, trace: &str, step: u64) -> anyhow::Result<crate::ResumeCursor> {
            self.backing.resume(trace, step).await
        }
        async fn complete(
            &self,
            trace: &str,
            completion: crate::CompletionState,
        ) -> anyhow::Result<()> {
            self.backing.complete(trace, completion).await?;
            if self.mode == 2 {
                anyhow::bail!("synthetic applied completion without ACK");
            }
            Ok(())
        }
        async fn save_intervention(
            &self,
            trace: &str,
            intervention: crate::persistence::Intervention,
        ) -> anyhow::Result<()> {
            self.backing.save_intervention(trace, intervention).await
        }
    }
    #[tokio::test]
    async fn actual_ack_matrix_blocks_initialization_and_retains_known_result() {
        use crate::PersistenceStore;
        for mode in 0..4 {
            for cancellable in [false, true] {
                for auto_complete in [false, true] {
                    if mode == 2 && !auto_complete {
                        continue;
                    }
                    let backing = crate::InMemoryPersistenceStore::new();
                    let original: Arc<dyn PersistenceStore> = Arc::new(AckStore {
                        backing: backing.clone(),
                        mode,
                    });
                    let effects = Arc::new(AtomicUsize::new(0));
                    let initialized = Arc::new(AtomicUsize::new(0));
                    let prefix = Axon::<usize, usize, String>::start("ack-handoff")
                        .then(Effect(effects.clone()));
                    let mut bus = Bus::new();
                    bus.insert(crate::PersistenceHandle::from_arc(original.clone()));
                    bus.insert(crate::PersistenceTraceId::new("ack-handoff"));
                    bus.insert(crate::PersistenceAutoComplete(auto_complete));
                    let report = prefix
                        .initialize_stream(
                            42,
                            &(),
                            &mut bus,
                            &Init(initialized.clone()),
                            cancellable.then(ranvier_core::CancellationToken::new),
                        )
                        .await;
                    match (mode, report) {
                        (
                            0,
                            StreamInitializationReport::Ready {
                                prefix: ExecutionReport::Recorded { completion, .. },
                                ..
                            },
                        ) => {
                            assert_eq!(
                                completion,
                                if auto_complete {
                                    CompletionConfirmation::Confirmed
                                } else {
                                    CompletionConfirmation::NotRequested
                                }
                            );
                            assert_eq!(initialized.load(Ordering::SeqCst), 1);
                        }
                        (
                            1,
                            StreamInitializationReport::Rejected {
                                prefix:
                                    ExecutionReport::BlockedBeforeEffect {
                                        operation: PersistenceOperation::Intent,
                                        effect: EffectBoundary::NotInvoked,
                                        ..
                                    },
                            },
                        ) => {}
                        (
                            2,
                            StreamInitializationReport::Rejected {
                                prefix:
                                    ExecutionReport::Unconfirmed {
                                        result: ExecutionTerminal::Outcome(Outcome::Next(42)),
                                        operation: PersistenceOperation::Complete,
                                        effect: EffectBoundary::MayHaveRun,
                                        ..
                                    },
                            },
                        ) => {
                            assert!(
                                backing
                                    .load("ack-handoff")
                                    .await
                                    .unwrap()
                                    .unwrap()
                                    .completion
                                    .is_some()
                            );
                        }
                        (
                            3,
                            StreamInitializationReport::Rejected {
                                prefix:
                                    ExecutionReport::BlockedBeforeEffect {
                                        operation: PersistenceOperation::Load,
                                        effect: EffectBoundary::NotInvoked,
                                        ..
                                    },
                            },
                        ) => {}
                        _ => panic!("unexpected admission state for mode {mode}"),
                    }
                    if mode != 0 {
                        assert_eq!(initialized.load(Ordering::SeqCst), 0);
                    }
                    assert_eq!(
                        effects.load(Ordering::SeqCst),
                        usize::from(mode == 0 || mode == 2)
                    );
                    assert!(Arc::ptr_eq(
                        &bus.read::<crate::PersistenceHandle>().unwrap().store(),
                        &original
                    ));
                }
            }
        }
    }

    struct PendingInit(Arc<tokio::sync::Notify>);
    #[async_trait::async_trait]
    impl StreamingTransition<usize> for PendingInit {
        type Item = usize;
        type Error = String;
        type Resources = ();
        async fn run_stream(
            &self,
            _: usize,
            _: &(),
            bus: &mut Bus,
        ) -> Result<Pin<Box<dyn futures_core::Stream<Item = usize> + Send>>, String> {
            bus.insert(SagaPolicy::Enabled);
            bus.insert(ranvier_core::iam::IamIdentity::new("initializer"));
            self.0.notify_one();
            std::future::pending().await
        }
    }
    #[tokio::test]
    async fn pending_initializer_cancel_and_outer_drop_restore_parent_context() {
        use ranvier_core::iam::IamIdentity;
        for cancel in [false, true] {
            let mut bus = Bus::new();
            let mut parent = crate::execution_observation::InvocationOwner::begin(&mut bus);
            parent.bus().insert_shared(IamIdentity::new("parent"));
            parent.bus().insert(SagaPolicy::Disabled);
            let identity = parent.bus().read::<IamIdentity>().unwrap() as *const _;
            let entered = Arc::new(tokio::sync::Notify::new());
            let token = ranvier_core::CancellationToken::new();
            let prefix = Axon::<usize, usize, String>::start("pending-init");
            let init = PendingInit(entered.clone());
            {
                let future =
                    prefix.initialize_stream(42, &(), parent.bus(), &init, Some(token.clone()));
                tokio::pin!(future);
                tokio::select! {
                    _ = entered.notified() => {},
                    _ = &mut future => panic!("pending init ended early"),
                    _ = tokio::time::sleep(std::time::Duration::from_secs(3)) => panic!("init did not enter"),
                }
                if cancel {
                    token.cancel(ranvier_core::CancellationReason::Explicit);
                    assert!(matches!(
                        future.await,
                        StreamInitializationReport::Cancelled {
                            prefix: ExecutionReport::Unpersisted { result: (), .. },
                            initialization: EffectBoundary::MayHaveRun,
                            ..
                        }
                    ));
                }
            }
            assert_eq!(
                parent.bus().read::<IamIdentity>().unwrap() as *const _,
                identity
            );
            assert_eq!(
                parent.bus().read::<SagaPolicy>(),
                Some(&SagaPolicy::Disabled)
            );
        }
    }
    #[test]
    fn value_mapping_never_invokes_callback_for_rejected_failed_or_cancelled() {
        let context = CancellationContext::new(ranvier_core::CancellationReason::Explicit);
        let cases: [StreamInitializationReport<u32, Box<u32>, String>; 4] = [
            StreamInitializationReport::Ready {
                prefix: ExecutionReport::Recorded {
                    result: (),
                    completion: CompletionConfirmation::NotRequested,
                    cleanup: None,
                },
                value: Box::new(42),
            },
            StreamInitializationReport::Rejected {
                prefix: ExecutionReport::Unconfirmed {
                    result: ExecutionTerminal::Outcome(Outcome::Next(42)),
                    operation: PersistenceOperation::Complete,
                    effect: EffectBoundary::MayHaveRun,
                    cleanup: None,
                },
            },
            StreamInitializationReport::Failed {
                prefix: ExecutionReport::Unpersisted {
                    result: (),
                    cleanup: None,
                },
                error: StreamInitializationError::Transition("synthetic".into()),
                initialization: EffectBoundary::MayHaveRun,
                initialization_cleanup: None,
            },
            StreamInitializationReport::Cancelled {
                prefix: ExecutionReport::NotRequested {
                    result: (),
                    cleanup: None,
                },
                context: context.clone(),
                initialization: EffectBoundary::NotInvoked,
                initialization_cleanup: None,
            },
        ];
        for (index, report) in cases.into_iter().enumerate() {
            let mut calls = 0;
            let mapped = report.map_value(|value| {
                calls += 1;
                vec![value]
            });
            assert_eq!(calls, usize::from(index == 0));
            match mapped {
                StreamInitializationReport::Ready {
                    prefix:
                        ExecutionReport::Recorded {
                            result: (),
                            completion: CompletionConfirmation::NotRequested,
                            cleanup: None,
                        },
                    value,
                } => assert_eq!(*value[0], 42),
                StreamInitializationReport::Rejected {
                    prefix:
                        ExecutionReport::Unconfirmed {
                            result: ExecutionTerminal::Outcome(Outcome::Next(42)),
                            operation: PersistenceOperation::Complete,
                            effect: EffectBoundary::MayHaveRun,
                            cleanup: None,
                        },
                } => assert_eq!(index, 1),
                StreamInitializationReport::Failed {
                    error: StreamInitializationError::Transition(error),
                    prefix:
                        ExecutionReport::Unpersisted {
                            result: (),
                            cleanup: None,
                        },
                    initialization: EffectBoundary::MayHaveRun,
                    initialization_cleanup: None,
                } => {
                    assert_eq!(index, 2);
                    assert_eq!(error, "synthetic");
                }
                StreamInitializationReport::Cancelled {
                    prefix:
                        ExecutionReport::NotRequested {
                            result: (),
                            cleanup: None,
                        },
                    context: actual,
                    initialization: EffectBoundary::NotInvoked,
                    initialization_cleanup: None,
                } => {
                    assert_eq!(index, 3);
                    assert_eq!(actual, context);
                }
                _ => panic!("mapping changed report state"),
            }
        }
    }
}
