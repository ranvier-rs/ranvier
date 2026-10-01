use crate::execution_report::{
    CompletionConfirmation, EffectBoundary, ExecutionReport, PersistenceOperation,
};
use crate::persistence::*;
use ranvier_core::Bus;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

#[derive(Default)]
pub(crate) struct Observation {
    failure: Mutex<Option<PersistenceOperation>>,
    effect: AtomicBool,
    attempted_write: AtomicBool,
    completed: AtomicBool,
    blocked: AtomicBool,
    encoding_failed: AtomicBool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::FutureExt;

    struct TestDlqSink;
    #[async_trait::async_trait]
    impl ranvier_core::event::DlqSink for TestDlqSink {
        async fn store_dead_letter(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: &str,
            _: &[u8],
        ) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn nested_token_restores_exact_storage_on_finish_or_drop() {
        use ranvier_core::iam::IamToken;
        for mode in 0..5 {
            for finish in [false, true] {
                let mut root = Bus::new();
                if mode == 1 {
                    root.insert(IamToken("parent-fixture-token".into()));
                }
                if mode >= 2 {
                    root.insert_shared(IamToken("parent-fixture-token".into()));
                }
                let mut fork = if mode >= 3 {
                    Some(root.fork_for_parallel())
                } else {
                    None
                };
                let bus = if mode == 4 {
                    fork.as_mut().unwrap()
                } else {
                    &mut root
                };
                let mut parent = InvocationOwner::begin(bus);
                let ptr = parent
                    .bus()
                    .read::<IamToken>()
                    .map(|v| v as *const IamToken);
                {
                    let mut child = InvocationOwner::begin(parent.bus());
                    if mode >= 3 {
                        assert!(child.bus().remove::<IamToken>().is_none());
                    }
                    child
                        .bus()
                        .insert_shared(IamToken("child-fixture-token".into()));
                    if finish {
                        let _ = child.finish(());
                    }
                }
                assert_eq!(
                    parent
                        .bus()
                        .read::<IamToken>()
                        .map(|v| v as *const IamToken),
                    ptr
                );
                assert_eq!(
                    parent.bus().read::<IamToken>().map(|v| v.0.as_str()),
                    if mode == 0 {
                        None
                    } else {
                        Some("parent-fixture-token")
                    }
                );
                assert_eq!(
                    parent
                        .bus()
                        .fork_for_parallel()
                        .read::<IamToken>()
                        .is_some(),
                    mode >= 2
                );
                drop(parent);
                if mode >= 3 {
                    assert_eq!(root.read::<IamToken>().unwrap().0, "parent-fixture-token");
                    assert_eq!(
                        fork.as_ref().unwrap().read::<IamToken>().unwrap().0,
                        "parent-fixture-token"
                    );
                }
            }
        }
    }

    #[test]
    fn nested_identity_restores_presence_storage_and_absence_after_finish_or_drop() {
        use ranvier_core::iam::IamIdentity;
        for present in [false, true] {
            for shared in [false, true] {
                for finish in [false, true] {
                    let mut bus = Bus::new();
                    let mut parent = InvocationOwner::begin(&mut bus);
                    if present {
                        let identity = IamIdentity::new("parent").with_role("parent");
                        if shared {
                            parent.bus().insert_shared(identity);
                        } else {
                            parent.bus().insert(identity);
                        }
                    }
                    let ptr = parent
                        .bus()
                        .read::<IamIdentity>()
                        .map(|v| v as *const IamIdentity);
                    {
                        let mut child = InvocationOwner::begin(parent.bus());
                        child.bus().insert_shared(IamIdentity::new("child"));
                        if finish {
                            let _ = child.finish(());
                        }
                    }
                    assert_eq!(
                        parent
                            .bus()
                            .read::<IamIdentity>()
                            .map(|v| v as *const IamIdentity),
                        ptr
                    );
                    assert_eq!(
                        parent
                            .bus()
                            .read::<IamIdentity>()
                            .map(|v| v.subject.as_str()),
                        if present { Some("parent") } else { None }
                    );
                    assert_eq!(
                        parent
                            .bus()
                            .fork_for_parallel()
                            .read::<IamIdentity>()
                            .is_some(),
                        present && shared
                    );
                }
            }
        }
    }

    #[test]
    fn nested_config_restores_values_and_absence_after_finish_or_drop() {
        use ranvier_core::{
            event::{DlqPolicy, DlqSink},
            saga::SagaPolicy,
            schematic::Schematic,
        };
        for configured in [false, true] {
            for finish in [false, true] {
                let mut bus = Bus::new();
                let mut parent = InvocationOwner::begin(&mut bus);
                let parent_sink: Arc<dyn DlqSink> = Arc::new(TestDlqSink);
                if configured {
                    parent.bus().insert(DlqPolicy::SendToDlq);
                    parent.bus().insert(SagaPolicy::Disabled);
                    parent
                        .bus()
                        .insert(crate::Axon::<i32, i32, String>::start("parent").schematic);
                    parent.bus().insert(parent_sink.clone());
                }
                {
                    let mut child = InvocationOwner::begin(parent.bus());
                    child.bus().insert(DlqPolicy::Drop);
                    child.bus().insert(SagaPolicy::Enabled);
                    child
                        .bus()
                        .insert(crate::Axon::<i32, i32, String>::start("child").schematic);
                    child
                        .bus()
                        .insert::<Arc<dyn DlqSink>>(Arc::new(TestDlqSink));
                    if finish {
                        let _ = child.finish(());
                    }
                }
                if configured {
                    assert_eq!(
                        parent.bus().read::<DlqPolicy>(),
                        Some(&DlqPolicy::SendToDlq)
                    );
                    assert_eq!(
                        parent.bus().read::<SagaPolicy>(),
                        Some(&SagaPolicy::Disabled)
                    );
                    assert_eq!(parent.bus().read::<Schematic>().unwrap().name, "parent");
                    assert!(Arc::ptr_eq(
                        parent.bus().read::<Arc<dyn DlqSink>>().unwrap(),
                        &parent_sink
                    ));
                } else {
                    assert!(parent.bus().read::<DlqPolicy>().is_none());
                    assert!(parent.bus().read::<SagaPolicy>().is_none());
                    assert!(parent.bus().read::<Schematic>().is_none());
                    assert!(parent.bus().read::<Arc<dyn DlqSink>>().is_none());
                }
            }
        }
    }

    #[test]
    fn invocation_restores_caller_access_guard_after_finish_or_drop() {
        use ranvier_core::{BusAccessError, BusAccessPolicy, BusTypeRef};
        for finish in [false, true] {
            let mut bus = Bus::new();
            bus.insert("private".to_string());
            bus.set_access_policy(
                "caller",
                Some(BusAccessPolicy::deny_only(vec![BusTypeRef::of::<String>()])),
            );
            {
                let mut owner = InvocationOwner::begin(&mut bus);
                assert!(owner.bus().get::<String>().is_ok());
                owner
                    .bus()
                    .set_access_policy("child", Some(BusAccessPolicy::allow_only(vec![])));
                if finish {
                    let _ = owner.finish(());
                }
            }
            assert!(
                matches!(bus.get::<String>(), Err(BusAccessError::Unauthorized { transition, .. }) if transition == "caller")
            );
            bus.clear_access_policy();
            assert!(bus.get::<String>().is_ok());
        }
    }

    #[test]
    fn owner_restores_caller_jump_and_removes_invocation_jump() {
        for finish in [false, true] {
            let mut bus = Bus::new();
            {
                let mut owner = InvocationOwner::begin(&mut bus);
                owner.bus().insert(crate::axon::ManualJump {
                    target_node: "generated".into(),
                    payload_override: Some(serde_json::json!(9)),
                });
                if finish {
                    let _ = owner.finish(());
                }
            }
            assert!(bus.read::<crate::axon::ManualJump>().is_none());
            bus.insert(crate::axon::ManualJump {
                target_node: "caller".into(),
                payload_override: Some(serde_json::json!(7)),
            });
            {
                let mut owner = InvocationOwner::begin(&mut bus);
                assert_eq!(
                    owner
                        .bus()
                        .read::<crate::axon::ManualJump>()
                        .unwrap()
                        .target_node,
                    "caller"
                );
                owner.bus().insert(crate::axon::ManualJump {
                    target_node: "replacement".into(),
                    payload_override: None,
                });
                {
                    let mut child = InvocationOwner::begin(owner.bus());
                    child.bus().insert(crate::axon::ManualJump {
                        target_node: "child".into(),
                        payload_override: Some(serde_json::json!(11)),
                    });
                    if finish {
                        let _ = child.finish(());
                    }
                }
                assert_eq!(
                    owner
                        .bus()
                        .read::<crate::axon::ManualJump>()
                        .unwrap()
                        .target_node,
                    "replacement"
                );
                if finish {
                    let _ = owner.finish(());
                }
            }
            let restored = bus.read::<crate::axon::ManualJump>().unwrap();
            assert_eq!(restored.target_node, "caller");
            assert_eq!(restored.payload_override, Some(serde_json::json!(7)));
            let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut owner = InvocationOwner::begin(&mut bus);
                owner.bus().insert(crate::axon::ManualJump {
                    target_node: "panicking".into(),
                    payload_override: None,
                });
                panic!("injected invocation unwind");
            }));
            assert!(unwind.is_err());
            let restored = bus.read::<crate::axon::ManualJump>().unwrap();
            assert_eq!(restored.target_node, "caller");
            assert_eq!(restored.payload_override, Some(serde_json::json!(7)));
        }
    }

    #[test]
    fn owner_restores_parent_checkpoint_position_and_removes_child_position() {
        let mut bus = Bus::new();
        {
            let mut parent = InvocationOwner::begin(&mut bus);
            parent.bus().insert(crate::axon::StartStep(7));
            parent.bus().insert(crate::axon::ResumptionState {
                payload: Some(serde_json::json!({"parent":7})),
            });
            {
                let mut child = InvocationOwner::begin(parent.bus());
                assert!(child.bus().read::<crate::axon::StartStep>().is_none());
                child.bus().insert(crate::axon::StartStep(9));
                assert!(child.bus().read::<crate::axon::ResumptionState>().is_none());
                child.bus().insert(crate::axon::ResumptionState {
                    payload: Some(serde_json::json!({"child":9})),
                });
            }
            assert_eq!(parent.bus().read::<crate::axon::StartStep>().unwrap().0, 7);
            let mut child = InvocationOwner::begin(parent.bus());
            child.bus().insert(crate::axon::ResumptionState {
                payload: Some(serde_json::json!({"finished_child":11})),
            });
            let _ = child.finish(());
            assert_eq!(
                parent
                    .bus()
                    .read::<crate::axon::ResumptionState>()
                    .unwrap()
                    .payload,
                Some(serde_json::json!({"parent":7}))
            );
        }
        assert!(bus.read::<crate::axon::StartStep>().is_none());
        assert!(bus.read::<crate::axon::ResumptionState>().is_none());
    }

    #[test]
    fn dropped_owner_removes_generated_timeline_and_preserves_caller_timeline() {
        let mut bus = Bus::new();
        {
            let mut owner = InvocationOwner::begin(&mut bus);
            owner.bus().insert(ranvier_core::timeline::Timeline::new());
            {
                let _child = InvocationOwner::begin(owner.bus());
            }
            assert!(
                owner
                    .bus()
                    .read::<ranvier_core::timeline::Timeline>()
                    .is_some()
            );
        }
        assert!(bus.read::<ranvier_core::timeline::Timeline>().is_none());
        bus.insert(ranvier_core::timeline::Timeline::new());
        {
            let _owner = InvocationOwner::begin(&mut bus);
        }
        assert!(bus.read::<ranvier_core::timeline::Timeline>().is_some());
    }

    #[test]
    fn invocation_owner_restores_caller_state_during_unwind() {
        let mut bus = Bus::new();
        bus.insert(PersistenceHandle::from_store(
            InMemoryPersistenceStore::new(),
        ));
        let original = bus.read::<PersistenceHandle>().unwrap().store();
        bus.insert(CompensationIdempotencyHandle::from_store(
            InMemoryCompensationIdempotencyStore::new(),
        ));
        let original_compensation = bus.read::<CompensationIdempotencyHandle>().unwrap().store();
        let parent = ranvier_core::cancellation::CancellationToken::new();
        bus.set_cancellation_token(parent.clone());
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut owner = InvocationOwner::begin(&mut bus);
            owner
                .bus()
                .set_cancellation_token(ranvier_core::cancellation::CancellationToken::new());
            assert!(owner.bus().read::<InvocationTrace>().is_some());
            panic!("injected invocation unwind");
        }));
        assert!(unwind.is_err());
        assert!(Arc::ptr_eq(
            &original,
            &bus.read::<PersistenceHandle>().unwrap().store()
        ));
        assert!(bus.read::<InvocationTrace>().is_none());
        assert!(bus.read::<ActiveObservation>().is_none());
        assert!(Arc::ptr_eq(
            &original_compensation,
            &bus.read::<CompensationIdempotencyHandle>().unwrap().store()
        ));
        parent.cancel(ranvier_core::cancellation::CancellationReason::Explicit);
        assert!(bus.cancellation_token().unwrap().is_cancelled());
    }

    #[test]
    fn dropped_child_owner_restores_parent_invocation() {
        let mut bus = Bus::new();
        bus.insert(PersistenceHandle::from_store(
            InMemoryPersistenceStore::new(),
        ));
        let original = bus.read::<PersistenceHandle>().unwrap().store();
        let mut parent = InvocationOwner::begin(&mut bus);
        let parent_trace = parent.bus().read::<InvocationTrace>().unwrap().0.clone();
        let parent_observation = parent.bus().read::<ActiveObservation>().unwrap().0.clone();
        {
            let mut child = InvocationOwner::begin(parent.bus());
            child.bus().read::<ActiveObservation>().unwrap().0.effect();
            assert_ne!(
                child.bus().read::<InvocationTrace>().unwrap().0,
                parent_trace
            );
        }
        assert_eq!(
            parent.bus().read::<InvocationTrace>().unwrap().0,
            parent_trace
        );
        assert!(!parent_observation.effect.load(Ordering::SeqCst));
        assert!(Arc::ptr_eq(
            &parent_observation,
            &parent.bus().read::<ActiveObservation>().unwrap().0
        ));
        let _ = parent.finish("parent result");
        assert!(Arc::ptr_eq(
            &original,
            &bus.read::<PersistenceHandle>().unwrap().store()
        ));
        assert!(bus.read::<InvocationTrace>().is_none());
        assert!(bus.read::<ActiveObservation>().is_none());
    }

    struct AppliedWithoutAck {
        backing: InMemoryCompensationIdempotencyStore,
        pending: bool,
    }
    #[async_trait::async_trait]
    impl CompensationIdempotencyStore for AppliedWithoutAck {
        async fn was_compensated(&self, key: &str) -> anyhow::Result<bool> {
            self.backing.was_compensated(key).await
        }
        async fn mark_compensated(&self, key: &str) -> anyhow::Result<()> {
            self.backing.mark_compensated(key).await?;
            if self.pending {
                std::future::pending::<()>().await;
            }
            Err(anyhow::anyhow!("private-error-sentinel"))
        }
    }

    async fn check_applied_write(pending: bool) {
        let backing = InMemoryCompensationIdempotencyStore::new();
        let mut bus = Bus::new();
        bus.insert(CompensationIdempotencyHandle::from_store(
            AppliedWithoutAck {
                backing: backing.clone(),
                pending,
            },
        ));
        let original = bus.read::<CompensationIdempotencyHandle>().unwrap().store();
        let session = Session::begin(&mut bus);
        session.observation.effect();
        let store = bus.read::<CompensationIdempotencyHandle>().unwrap().store();
        assert!(!store.was_compensated("key").await.unwrap());
        if pending {
            // Poll through the applied write once, then drop before any ACK.
            assert!(store.mark_compensated("key").now_or_never().is_none());
        } else {
            assert!(store.mark_compensated("key").await.is_err());
        }
        let report = session.finish(&mut bus, "domain-result");
        assert_eq!(
            report,
            ExecutionReport::Unconfirmed {
                result: "domain-result",
                operation: PersistenceOperation::CompensationWrite,
                effect: EffectBoundary::MayHaveRun,
                cleanup: None,
            }
        );
        assert!(backing.was_compensated("key").await.unwrap());
        let restored = bus.read::<CompensationIdempotencyHandle>().unwrap();
        assert!(Arc::ptr_eq(&restored.store(), &original));
        assert!(restored.store().was_compensated("key").await.unwrap());
        assert!(
            !serde_json::to_string(&report)
                .unwrap()
                .contains("private-error-sentinel")
        );
    }

    #[test]
    fn nested_session_restores_parent_cleanup_journal() {
        use crate::saga_report::{SagaRollbackJournal, SagaRollbackOutcome, SagaRollbackReport};
        let mut bus = Bus::new();
        let outer = Session::begin(&mut bus);
        let parent = SagaRollbackJournal::new("parent");
        parent.push("parent-node".into(), SagaRollbackOutcome::InProgress);
        bus.insert(parent.clone());
        let parent_snapshot = parent.snapshot();
        bus.insert(parent_snapshot.clone());
        let inner = Session::begin(&mut bus);
        assert!(bus.read::<SagaRollbackJournal>().is_none());
        let child = SagaRollbackJournal::new("child");
        child.push("child-node".into(), SagaRollbackOutcome::Fault);
        child.finish();
        bus.insert(child.snapshot());
        bus.insert(child);
        let inner_result = inner.finish(&mut bus, "child-result");
        let child_cleanup = inner_result.cleanup().unwrap();
        assert_eq!(child_cleanup.trace_id, "child");
        assert_eq!(child_cleanup.steps[0].outcome, SagaRollbackOutcome::Fault);
        assert!(!child_cleanup.is_complete());
        assert_eq!(bus.read::<SagaRollbackReport>().unwrap(), &parent_snapshot);
        parent.settle(SagaRollbackOutcome::Completed);
        assert_eq!(
            bus.read::<SagaRollbackJournal>().unwrap().snapshot(),
            parent.snapshot()
        );
        assert_eq!(
            bus.read::<SagaRollbackJournal>()
                .unwrap()
                .snapshot()
                .trace_id,
            "parent"
        );
        let outer_result = outer.finish(&mut bus, "parent-result");
        assert_eq!(outer_result.cleanup().unwrap().trace_id, "parent");
        assert_eq!(inner_result.cleanup().unwrap().trace_id, "child");
    }

    #[tokio::test]
    async fn applied_compensation_write_error_preserves_readback_and_report() {
        check_applied_write(false).await;
    }

    #[tokio::test]
    async fn dropped_compensation_write_preserves_readback_and_report() {
        check_applied_write(true).await;
    }
}

impl Observation {
    pub(crate) fn encoding_failed(&self, operation: PersistenceOperation) {
        self.encoding_failed.store(true, Ordering::SeqCst);
        self.failed(operation);
    }
    pub(crate) fn has_encoding_failure(&self) -> bool {
        self.encoding_failed.load(Ordering::SeqCst)
    }
    fn failed(&self, operation: PersistenceOperation) {
        let mut failure = self
            .failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if failure.is_none() {
            *failure = Some(operation);
        }
    }
    fn observe<T>(&self, operation: PersistenceOperation, result: &anyhow::Result<T>) {
        if result.is_err() {
            self.failed(operation);
        }
    }
    pub(crate) fn effect(&self) {
        self.effect.store(true, Ordering::SeqCst);
    }
    pub(crate) fn block(&self, operation: PersistenceOperation) {
        self.failed(operation);
        self.blocked.store(true, Ordering::SeqCst);
    }
    pub(crate) fn is_blocked(&self) -> bool {
        self.blocked.load(Ordering::SeqCst)
    }
    fn report<T>(&self, result: T) -> ExecutionReport<T> {
        let operation = *self
            .failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(operation) = operation {
            if self.blocked.load(Ordering::SeqCst) {
                return ExecutionReport::BlockedBeforeEffect {
                    operation,
                    effect: if self.effect.load(Ordering::SeqCst) {
                        EffectBoundary::MayHaveRun
                    } else {
                        EffectBoundary::NotInvoked
                    },
                    cleanup: None,
                };
            }
            return ExecutionReport::Unconfirmed {
                result,
                operation,
                effect: if self.effect.load(Ordering::SeqCst) {
                    EffectBoundary::MayHaveRun
                } else {
                    EffectBoundary::NotInvoked
                },
                cleanup: None,
            };
        }
        if !self.attempted_write.load(Ordering::SeqCst) {
            return ExecutionReport::NotRequested {
                result,
                cleanup: None,
            };
        }
        ExecutionReport::Recorded {
            result,
            completion: if self.completed.load(Ordering::SeqCst) {
                CompletionConfirmation::Confirmed
            } else {
                CompletionConfirmation::NotRequested
            },
            cleanup: None,
        }
    }
}

// Dropping a polled store future cannot establish non-commit or acknowledgement.
struct ObservedCall<'a> {
    observation: &'a Observation,
    operation: PersistenceOperation,
    settled: bool,
}
impl<'a> ObservedCall<'a> {
    fn new(observation: &'a Observation, operation: PersistenceOperation) -> Self {
        Self {
            observation,
            operation,
            settled: false,
        }
    }
    fn finish<T>(mut self, result: &anyhow::Result<T>) {
        self.observation.observe(self.operation, result);
        self.settled = true;
    }
}
impl Drop for ObservedCall<'_> {
    fn drop(&mut self) {
        if !self.settled {
            self.observation.failed(self.operation);
        }
    }
}

pub(crate) struct ObservedStore {
    pub(crate) inner: Arc<dyn PersistenceStore>,
    pub(crate) observation: Arc<Observation>,
}
#[async_trait::async_trait]
impl PersistenceStore for ObservedStore {
    async fn append(&self, envelope: PersistenceEnvelope) -> anyhow::Result<()> {
        let operation = if envelope.outcome_kind == "Enter" {
            PersistenceOperation::Intent
        } else if envelope.node_id.is_none() {
            PersistenceOperation::Terminal
        } else {
            PersistenceOperation::Result
        };
        self.observation
            .attempted_write
            .store(true, Ordering::SeqCst);
        let call = ObservedCall::new(&self.observation, operation);
        let result = self.inner.append(envelope).await;
        call.finish(&result);
        result
    }
    async fn load(&self, trace: &str) -> anyhow::Result<Option<PersistedTrace>> {
        let call = ObservedCall::new(&self.observation, PersistenceOperation::Load);
        let result = self.inner.load(trace).await;
        call.finish(&result);
        result
    }
    async fn resume(&self, trace: &str, step: u64) -> anyhow::Result<ResumeCursor> {
        let call = ObservedCall::new(&self.observation, PersistenceOperation::Load);
        let result = self.inner.resume(trace, step).await;
        call.finish(&result);
        result
    }
    async fn complete(&self, trace: &str, completion: CompletionState) -> anyhow::Result<()> {
        self.observation
            .attempted_write
            .store(true, Ordering::SeqCst);
        let call = ObservedCall::new(&self.observation, PersistenceOperation::Complete);
        let result = self.inner.complete(trace, completion).await;
        call.finish(&result);
        if result.is_ok() {
            self.observation.completed.store(true, Ordering::SeqCst);
        }
        result
    }
    async fn save_intervention(
        &self,
        trace: &str,
        intervention: Intervention,
    ) -> anyhow::Result<()> {
        self.observation
            .attempted_write
            .store(true, Ordering::SeqCst);
        let call = ObservedCall::new(&self.observation, PersistenceOperation::Result);
        let result = self.inner.save_intervention(trace, intervention).await;
        call.finish(&result);
        result
    }
}

#[derive(Clone)]
pub(crate) struct ActiveObservation(pub(crate) Arc<Observation>);

pub(crate) struct ObservedCompensationStore {
    pub(crate) inner: Arc<dyn CompensationIdempotencyStore>,
    pub(crate) observation: Arc<Observation>,
}
#[async_trait::async_trait]
impl CompensationIdempotencyStore for ObservedCompensationStore {
    async fn was_compensated(&self, key: &str) -> anyhow::Result<bool> {
        let call = ObservedCall::new(&self.observation, PersistenceOperation::CompensationRead);
        let result = self.inner.was_compensated(key).await;
        call.finish(&result);
        result
    }
    async fn mark_compensated(&self, key: &str) -> anyhow::Result<()> {
        self.observation
            .attempted_write
            .store(true, Ordering::SeqCst);
        let call = ObservedCall::new(&self.observation, PersistenceOperation::CompensationWrite);
        let result = self.inner.mark_compensated(key).await;
        call.finish(&result);
        result
    }
}

#[derive(Clone)]
pub(crate) struct InvocationTrace(pub(crate) String);

#[cfg(test)]
mod invocation_trace_tests {
    use super::{InvocationTrace, Session};
    use ranvier_core::Bus;

    #[test]
    fn invocation_identity_is_stable_unique_and_restored_after_nested_finish() {
        let mut bus = Bus::new();
        let parent = Session::begin(&mut bus);
        let parent_id = bus.read::<InvocationTrace>().unwrap().0.clone();
        assert_eq!(bus.read::<InvocationTrace>().unwrap().0, parent_id);
        let child = Session::begin(&mut bus);
        let child_id = bus.read::<InvocationTrace>().unwrap().0.clone();
        assert_ne!(parent_id, child_id);
        let _ = child.finish(&mut bus, ());
        assert_eq!(bus.read::<InvocationTrace>().unwrap().0, parent_id);
        let _ = parent.finish(&mut bus, ());
        assert!(bus.read::<InvocationTrace>().is_none());
        let next = Session::begin(&mut bus);
        assert_ne!(bus.read::<InvocationTrace>().unwrap().0, parent_id);
        let _ = next.finish(&mut bus, ());
        assert!(bus.read::<InvocationTrace>().is_none());
    }
}

pub(crate) struct Session {
    original: Option<PersistenceHandle>,
    original_compensation: Option<CompensationIdempotencyHandle>,
    previous: Option<ActiveObservation>,
    previous_trace: Option<InvocationTrace>,
    previous_identity: Option<ranvier_core::bus::BusResourceScope>,
    previous_token: Option<ranvier_core::bus::BusResourceScope>,
    previous_dlq_policy: Option<ranvier_core::event::DlqPolicy>,
    previous_dlq_sink: Option<Arc<dyn ranvier_core::event::DlqSink>>,
    previous_schematic: Option<ranvier_core::schematic::Schematic>,
    previous_saga_policy: Option<ranvier_core::saga::SagaPolicy>,
    previous_cleanup: Option<crate::saga_report::SagaRollbackJournal>,
    previous_cleanup_report: Option<crate::saga_report::SagaRollbackReport>,
    observation: Arc<Observation>,
}
impl Session {
    pub(crate) fn begin(bus: &mut Bus) -> Self {
        let original = bus.read::<PersistenceHandle>().cloned();
        let original_compensation = bus.read::<CompensationIdempotencyHandle>().cloned();
        let previous = bus.read::<ActiveObservation>().cloned();
        let previous_identity = previous
            .as_ref()
            .map(|_| bus.begin_resource_scope::<ranvier_core::iam::IamIdentity>());
        let previous_token = previous
            .as_ref()
            .map(|_| bus.begin_resource_scope::<ranvier_core::iam::IamToken>());
        let previous_dlq_policy = bus.read::<ranvier_core::event::DlqPolicy>().cloned();
        let previous_dlq_sink = bus.read::<Arc<dyn ranvier_core::event::DlqSink>>().cloned();
        let previous_schematic = bus.read::<ranvier_core::schematic::Schematic>().cloned();
        let previous_saga_policy = bus.read::<ranvier_core::saga::SagaPolicy>().cloned();
        let previous_trace = bus.remove::<InvocationTrace>();
        bus.insert(InvocationTrace(format!(
            "{}:{}",
            bus.id,
            uuid::Uuid::new_v4()
        )));
        let cleanup_report = bus.remove::<crate::saga_report::SagaRollbackReport>();
        let cleanup = bus.remove::<crate::saga_report::SagaRollbackJournal>();
        let (previous_cleanup, previous_cleanup_report) = if previous.is_some() {
            (cleanup, cleanup_report)
        } else {
            (None, None)
        };
        let observation = Arc::new(Observation::default());
        if let Some(handle) = &original {
            bus.insert(handle.observed(observation.clone()));
        }
        if let Some(handle) = &original_compensation {
            bus.insert(handle.observed(observation.clone()));
        }
        bus.insert(ActiveObservation(observation.clone()));
        Self {
            original,
            original_compensation,
            previous,
            previous_trace,
            previous_identity,
            previous_token,
            previous_dlq_policy,
            previous_dlq_sink,
            previous_schematic,
            previous_saga_policy,
            previous_cleanup,
            previous_cleanup_report,
            observation,
        }
    }
    #[cfg(test)]
    pub(crate) fn finish<T>(self, bus: &mut Bus, result: T) -> ExecutionReport<T> {
        let report = self.build_report(bus, result);
        self.restore(bus);
        report
    }

    fn build_report<T>(&self, bus: &mut Bus, result: T) -> ExecutionReport<T> {
        bus.clear_access_policy();
        // Capture child ownership before restoring a parent's Bus observations.
        let cleanup = bus
            .read::<crate::saga_report::SagaRollbackJournal>()
            .map(|journal| journal.snapshot())
            .or_else(|| {
                bus.read::<crate::saga_report::SagaRollbackReport>()
                    .cloned()
            });
        let report = if self.original.is_some() || self.original_compensation.is_some() {
            self.observation.report(result)
        } else {
            ExecutionReport::Unpersisted {
                result,
                cleanup: None,
            }
        }
        .with_cleanup(cleanup);
        report
    }

    fn restore(self, bus: &mut Bus) {
        bus.clear_access_policy();
        if let Some(original) = self.original {
            bus.insert(original);
        }
        if let Some(original) = self.original_compensation {
            bus.insert(original);
        }
        let _ = bus.remove::<InvocationTrace>();
        if let Some(trace) = self.previous_trace {
            bus.insert(trace);
        }
        if let Some(scope) = self.previous_token {
            bus.restore_resource_scope(scope);
        }
        if let Some(scope) = self.previous_identity {
            bus.restore_resource_scope(scope);
        }
        if let Some(previous) = self.previous {
            // Child execution config must not become the parent's final policy
            // or attribution. Restore absence as well as configured values.
            bus.remove::<ranvier_core::event::DlqPolicy>();
            bus.remove::<Arc<dyn ranvier_core::event::DlqSink>>();
            bus.remove::<ranvier_core::schematic::Schematic>();
            bus.remove::<ranvier_core::saga::SagaPolicy>();
            if let Some(value) = self.previous_dlq_policy {
                bus.insert(value);
            }
            if let Some(value) = self.previous_dlq_sink {
                bus.insert(value);
            }
            if let Some(value) = self.previous_schematic {
                bus.insert(value);
            }
            if let Some(value) = self.previous_saga_policy {
                bus.insert(value);
            }
            bus.insert(previous);
            let _ = bus.remove::<crate::saga_report::SagaRollbackJournal>();
            let _ = bus.remove::<crate::saga_report::SagaRollbackReport>();
            if let Some(journal) = self.previous_cleanup {
                bus.insert(journal);
            }
            if let Some(report) = self.previous_cleanup_report {
                bus.insert(report);
            }
        } else {
            let _ = bus.remove::<ActiveObservation>();
        }
    }
}

/// Exact control-plane state suspended across a framework initialization boundary.
struct ControlScope {
    access: ranvier_core::bus::BusAccessScope,
    token: Option<ranvier_core::cancellation::CancellationToken>,
}
impl ControlScope {
    fn begin(bus: &mut Bus) -> Self {
        Self {
            access: bus.take_access_scope(),
            token: bus.cancellation_token().cloned(),
        }
    }
    fn restore(self, bus: &mut Bus) {
        bus.replace_cancellation_token(self.token);
        bus.restore_access_scope(self.access);
    }
}

/// Owns only streaming initialization control state, without constructing or
/// discarding a persistence report. The returned stream owns its cancellation
/// token independently and cannot borrow the caller Bus.
#[cfg(feature = "streaming")]
pub(crate) struct StreamInitializationOwner<'a> {
    bus: &'a mut Bus,
    control: Option<ControlScope>,
}
#[cfg(feature = "streaming")]
impl<'a> StreamInitializationOwner<'a> {
    pub(crate) fn begin(bus: &'a mut Bus) -> Self {
        let control = ControlScope::begin(bus);
        Self {
            bus,
            control: Some(control),
        }
    }
    pub(crate) fn bus(&mut self) -> &mut Bus {
        self.bus
    }
}
#[cfg(feature = "streaming")]
impl Drop for StreamInitializationOwner<'_> {
    fn drop(&mut self) {
        if let Some(control) = self.control.take() {
            control.restore(self.bus);
        }
    }
}

/// Owns the Bus borrow until the executor future and its child borrows are dropped.
pub(crate) struct InvocationOwner<'a> {
    bus: &'a mut Bus,
    session: Option<Session>,
    control: Option<ControlScope>,
    caller_timeline: bool,
    start_step: Option<crate::axon::StartStep>,
    resumption: Option<crate::axon::ResumptionState>,
    manual_jump: Option<crate::axon::ManualJump>,
}

impl<'a> InvocationOwner<'a> {
    pub(crate) fn begin(bus: &'a mut Bus) -> Self {
        // Framework setup cannot run under a caller transition's guard. Own the
        // exact suspended guard and restore it only after all framework cleanup.
        let control = ControlScope::begin(bus);
        let caller_timeline = bus.read::<ranvier_core::timeline::Timeline>().is_some();
        let start_step = bus.remove::<crate::axon::StartStep>();
        let resumption = bus.remove::<crate::axon::ResumptionState>();
        // ManualJump is also a public caller input. Keep it visible during this
        // invocation, but restore it if persisted intervention replaces it.
        let manual_jump = bus.read::<crate::axon::ManualJump>().cloned();
        let session = Session::begin(bus);
        Self {
            bus,
            session: Some(session),
            control: Some(control),
            caller_timeline,
            start_step,
            resumption,
            manual_jump,
        }
    }
    pub(crate) fn bus(&mut self) -> &mut Bus {
        self.bus
    }
    pub(crate) fn finish<T>(mut self, result: T) -> ExecutionReport<T> {
        let report = match self.session.as_ref() {
            Some(session) => session.build_report(self.bus, result),
            None => unreachable!("invocation session is consumed only by finish"),
        };
        if let Some(session) = self.session.take() {
            session.restore(self.bus);
        }
        report
    }
}

impl Drop for InvocationOwner<'_> {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            session.restore(self.bus);
        }
        let _ = self.bus.remove::<crate::axon::StartStep>();
        let _ = self.bus.remove::<crate::axon::ResumptionState>();
        let _ = self.bus.remove::<crate::axon::ManualJump>();
        if let Some(jump) = self.manual_jump.take() {
            self.bus.insert(jump);
        }
        if let Some(start) = self.start_step.take() {
            self.bus.insert(start);
        }
        if let Some(resumption) = self.resumption.take() {
            self.bus.insert(resumption);
        }
        if !self.caller_timeline {
            let _ = self.bus.remove::<ranvier_core::timeline::Timeline>();
        }
        if let Some(control) = self.control.take() {
            control.restore(self.bus);
        }
    }
}
