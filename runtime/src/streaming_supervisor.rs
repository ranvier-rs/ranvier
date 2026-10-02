//! Paired in-process reservations before allocating an owned stream context.
//! Capacity counts owners, not bytes. Reservations alone do not prove durability.
use crate::nested_custody::{CustodyReservation, CustodyStore, WeakCustodyClient};
use crate::streaming_axon::OwnedStreamContext;
use crate::streaming_recovery_admission::{RecoveryPermit, StreamRecoveryAdmission};

/// Root owner for finite stream admission and live recovery records.
/// Keep this owner alive through execution and explicit drain/retirement.
/// Dropping it does not run asynchronous cleanup or reconcile external effects.
pub struct StreamSupervisor {
    pub(crate) admission: StreamRecoveryAdmission,
    pub(crate) root: CustodyStore,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamReservationError {
    RecoveryCapacity,
    RecoveryClosed,
    RecoveryUnavailable,
    RootCapacity,
    RootClosed,
    RootUnavailable,
}
/// Admission failure has no factory result; factory rejection retains its exact X.
pub enum StreamContextFactoryError<X> {
    Admission(StreamReservationError),
    Factory(X),
    AlreadyCreated,
}
impl<X> std::fmt::Debug for StreamContextFactoryError<X> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Admission(reason) => f.debug_tuple("Admission").field(reason).finish(),
            Self::Factory(_) => f.write_str("StreamContextFactoryError::Factory"),
            Self::AlreadyCreated => f.write_str("StreamContextFactoryError::AlreadyCreated"),
        }
    }
}
fn root_error(signal: &'static str) -> StreamReservationError {
    match signal {
        "execution.nested_custody_capacity" => StreamReservationError::RootCapacity,
        "execution.nested_custody_admission_closed" | "execution.parent_custody_released" => StreamReservationError::RootClosed,
        _ => StreamReservationError::RootUnavailable,
    }
}
fn recovery_error(error: crate::streaming_recovery_admission::RecoveryAdmissionFailure) -> StreamReservationError {
    use crate::streaming_recovery_admission::RecoveryAdmissionFailure as Failure;
    match error {
        Failure::Capacity => StreamReservationError::RecoveryCapacity,
        Failure::Closed => StreamReservationError::RecoveryClosed,
        _ => StreamReservationError::RecoveryUnavailable,
    }
}
/// Unique reservation of one recovery owner and one root record.
/// Drop refunds undecided slots. This is neither cloneable nor serializable.
pub struct StreamReservation {
    pub(crate) admission: StreamRecoveryAdmission,
    pub(crate) permit: RecoveryPermit,
    pub(crate) client: WeakCustodyClient,
    pub(crate) slot: CustodyReservation,
    context_created: bool,
}
impl StreamSupervisor {
    pub fn new(capacity: usize) -> Self {
        Self { admission: StreamRecoveryAdmission::new(capacity), root: CustodyStore::new(capacity) }
    }
    /// Reserve both finite capacities before invoking request/context callbacks.
    /// Partial failure refunds the first reservation before returning.
    pub fn reserve(&self) -> Result<StreamReservation, StreamReservationError> {
        let permit = self.admission.reserve().map_err(recovery_error)?;
        let slot = self.root.reserve_stream_slot().map_err(root_error)?;
        Ok(StreamReservation { admission: self.admission.clone(), permit, client: self.root.client(), slot, context_created: false })
    }
    /// Stop both new admissions. Existing records remain retained for draining.
    /// Close attempts both authorities even when one is unavailable.
    pub fn close(&self) -> Result<(), StreamReservationError> {
        let recovery = self.admission.close().map_err(|_| StreamReservationError::RecoveryUnavailable);
        let root = self.root.close_admission().map_err(|_| StreamReservationError::RootUnavailable);
        recovery.and(root)
    }
}
impl StreamReservation {
    /// Invoke one context factory only after paired admission succeeded.
    /// A successful ticket cannot be reused for another context factory.
    /// Factory errors or panic unwind refund both reservations. Successful return
    /// retains the unique ticket alongside the exact context for later binding.
    /// Authorities are rechecked before the callback, without holding locks over
    /// user code. A concurrent close after that check may let an admitted callback
    /// finish; later frame binding still rejects closed root admission.
    pub fn with_context<Res, X>(mut self, factory: impl FnOnce() -> Result<OwnedStreamContext<Res>, X>)
        -> Result<(Self, OwnedStreamContext<Res>), StreamContextFactoryError<X>> {
        if self.context_created { return Err(StreamContextFactoryError::AlreadyCreated); }
        self.context_created = true;
        self.admission.validate_open_permit(&self.permit)
            .map_err(|error| StreamContextFactoryError::Admission(recovery_error(error)))?;
        self.slot.validate_open(&self.client)
            .map_err(|signal| StreamContextFactoryError::Admission(root_error(signal)))?;
        let context = factory().map_err(StreamContextFactoryError::Factory)?;
        Ok((self, context))
    }
}

/// Setup rejection preserves the original request context and input. Later
/// preparation rejection retains an opaque recovery lease instead of discarding it.
pub enum StreamPrepareError<In, Res> {
    Setup { signal: &'static str, input: In, context: OwnedStreamContext<Res> },
    Preparation { signal: &'static str, input: In, recovery: StreamRecoveryLease },
}
/// In-process owner lease only; Drop does not reconcile external effects.
pub struct StreamRecoveryLease { _owner: crate::nested_custody::Capability }
type PreparedFrame<Out, E> = crate::managed_stream_task::PreparedStreamTask<Result<
    crate::StreamInitializationReport<Out, crate::streaming_recovery::StreamReadyResult<()>, E>,
    crate::streaming_recovery::StreamInvocationAlreadyStarted>>;
/// Prepared exact frame, with no public raw start permit.
pub struct PreparedSupervisedStream<In, Out, Item, E, Res> {
    plan: crate::streaming_axon::OwnedCallablePlan<In, Out, Item, E, Res>,
    prepared: PreparedFrame<Out, E>,
    slot: CustodyReservation,
    client: WeakCustodyClient,
    items: tokio::sync::mpsc::Receiver<Item>,
    readiness: crate::streaming_axon::StreamReadiness,
}
/// Actual invocation, item transport and original typed report have distinct roles.
/// Keep the supervisor alive; dropping this value requests cancellation, while
/// the root and report leases retain recovery ownership for later settlement.
pub struct SupervisedStreamInvocation<Out, Item, E> {
    pub items: tokio::sync::mpsc::Receiver<Item>,
    pub readiness: crate::streaming_axon::StreamReadiness,
    pub report: crate::streaming_report_handle::StreamReportHandle<Out, E>,
    control: ranvier_core::CancellationToken,
    _custody: crate::nested_custody::WeakCustody,
}
impl<Out, Item, E> SupervisedStreamInvocation<Out, Item, E> {
    pub fn cancel(&self, reason: ranvier_core::CancellationReason) { self.control.cancel(reason); }
}
impl<Out, Item, E> Drop for SupervisedStreamInvocation<Out, Item, E> {
    fn drop(&mut self) { self.control.cancel(ranvier_core::CancellationReason::ClientDisconnected); }
}
/// Rejected start still exposes bounded access to the exact rejected/failed frame.
pub struct StreamStartRejected<Out, E> {
    pub signal: &'static str,
    pub report: crate::streaming_report_handle::StreamReportHandle<Out, E>,
    _custody: Option<crate::nested_custody::WeakCustody>,
}
impl StreamReservation {
    pub fn prepare<In, Out, Item, E, Res>(self,
        circuit: &crate::streaming_axon::StreamingAxon<In, Out, Item, E, Res>,
        context: OwnedStreamContext<Res>, input: In, cleanup_grace: std::time::Duration,
        expires: Option<std::time::Instant>,
    ) -> Result<PreparedSupervisedStream<In, Out, Item, E, Res>, StreamPrepareError<In, Res>>
    where In: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        Out: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
        Item: Send + 'static, Res: Send + Sync + 'static,
    {
        if let Err(signal) = self.slot.validate_open(&self.client) {
            return Err(StreamPrepareError::Setup { signal, context, input });
        }
        let Self { admission, permit, client, slot, .. } = self;
        let mut plan = match circuit.owned_definition.try_callable_plan_reserved(context, &admission, expires, permit) {
            Ok(plan) => plan,
            Err(error) => {
                let (signal, context) = match error {
                    crate::streaming_axon::OwnedStreamPlanRejected::InitializerType { context } =>
                        ("execution.stream_initializer_mismatch", context),
                    crate::streaming_axon::OwnedStreamPlanRejected::Admission(error) => {
                        let (_, context) = error.into_parts();
                        ("execution.stream_recovery_admission_rejected", context)
                    }
                };
                return Err(StreamPrepareError::Setup { signal, input, context });
            }
        };
        let ((prepared, items), readiness) = match plan.prepare_bounded_ready(input, cleanup_grace) {
            Ok(frame) => frame,
            Err(error) => return Err(StreamPrepareError::Preparation {
                signal: error.signal, input: error.input,
                recovery: StreamRecoveryLease { _owner: plan.owner.retention_capability() },
            }),
        };
        Ok(PreparedSupervisedStream { plan, prepared, slot, client, items, readiness })
    }
}
impl<In, Out, Item, E, Res> PreparedSupervisedStream<In, Out, Item, E, Res>
where In: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
    Out: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
    E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    Res: Send + Sync + 'static,
{
    pub async fn start(mut self, parent: ranvier_core::CancellationToken)
        -> Result<SupervisedStreamInvocation<Out, Item, E>, StreamStartRejected<Out, E>> {
        let report = crate::streaming_report_handle::StreamReportHandle::new(
            self.prepared.holder(), self.plan.owner.retention_capability());
        let control = parent.child_token();
        let journal = crate::SagaRollbackJournal::new("supervised-stream-root");
        let custody = match self.plan.owner.register_reserved_and_start(self.client, &mut self.slot,
            &journal, control.clone(), &mut self.prepared).await {
            Ok(custody) => custody,
            Err(error) => return Err(StreamStartRejected { signal: error.signal, report, _custody: error.custody }),
        };
        Ok(SupervisedStreamInvocation { items: self.items, readiness: self.readiness, report, control, _custody: custody })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
    #[derive(Clone)]
    struct Finite;
    #[async_trait::async_trait]
    impl ranvier_core::streaming::StreamingTransition<u32> for Finite {
        type Item = u32; type Error = String; type Resources = ();
        async fn run_stream(&self, input: u32, _: &(), _: &mut ranvier_core::Bus)
            -> Result<std::pin::Pin<Box<dyn futures_core::Stream<Item = u32> + Send>>, String> {
            Ok(Box::pin(futures_util::stream::iter([input])))
        }
    }
    #[derive(Clone)]
    struct Prefix;
    #[async_trait::async_trait]
    impl ranvier_core::Transition<u32, u32> for Prefix {
        type Error = String; type Resources = ();
        async fn run(&self, input: u32, _: &(), _: &mut ranvier_core::Bus) -> ranvier_core::Outcome<u32, String> {
            ranvier_core::Outcome::Next(input)
        }
    }
    #[tokio::test]
    async fn public_invocation_transfers_original_item_and_report_at_capacity_one() {
        let supervisor = StreamSupervisor::new(1);
        let circuit = crate::Axon::<u32, u32, String>::start("public-owned").then(Prefix).then_stream(Finite);
        let (ticket, context) = supervisor.reserve().unwrap().with_context(||
            OwnedStreamContext::try_new(Arc::new(()), ranvier_core::Bus::new())).unwrap();
        let prepared = match ticket.prepare(&circuit, context, 233, std::time::Duration::from_secs(1), None) {
            Ok(prepared) => prepared, Err(_) => panic!("setup rejected"),
        };
        let parent = ranvier_core::CancellationToken::new();
        let mut invocation = prepared.start(parent.clone()).await.unwrap_or_else(|e| panic!("{}", e.signal));
        tokio::time::timeout(std::time::Duration::from_secs(5), invocation.readiness.initialized()).await.unwrap().unwrap();
        assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(5), invocation.items.recv()).await.unwrap(), Some(233));
        assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(5), invocation.items.recv()).await.unwrap(), None);
        let report = invocation.report.take_report_until(tokio::time::Instant::now() + std::time::Duration::from_secs(5)).await.unwrap();
        assert!(matches!(report, crate::streaming_report_handle::SupervisedStreamCompletion::Report(
            crate::StreamInitializationReport::Ready { value, .. })
            if matches!(value.consumption, Ok(crate::StreamConsumptionTerminal::Completed))));
        assert!(supervisor.reserve().is_err());
        drop(invocation);
        assert!(!parent.is_cancelled());
    }
    #[test]
    fn paired_reservation_precedes_factory_and_refunds_failure() {
        let supervisor = StreamSupervisor::new(1);
        let calls = AtomicUsize::new(0);
        let ticket = supervisor.reserve().unwrap();
        assert!(matches!(supervisor.reserve(), Err(StreamReservationError::RecoveryCapacity)));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let result = ticket.with_context::<(), _>(|| {
            calls.fetch_add(1, Ordering::SeqCst); Err("original factory error")
        });
        assert!(matches!(result, Err(StreamContextFactoryError::Factory("original factory error"))));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(supervisor.admission.in_use(), Ok(0));
        assert!(supervisor.reserve().is_ok());
    }
    #[test]
    fn closed_or_released_root_does_not_call_context_factory() {
        for released in [false, true] {
            let supervisor = StreamSupervisor::new(1);
            let ticket = supervisor.reserve().unwrap();
            let admission = supervisor.admission.clone();
            if !released { supervisor.close().unwrap(); }
            drop(supervisor);
            let calls = AtomicUsize::new(0);
            let result = ticket.with_context(|| {
                calls.fetch_add(1, Ordering::SeqCst);
                OwnedStreamContext::try_new(Arc::new(()), ranvier_core::Bus::new())
            });
            assert!(result.is_err());
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert_eq!(admission.in_use(), Ok(0));
        }
    }
    #[test]
    fn partial_root_failure_and_factory_unwind_refund_recovery() {
        let supervisor = StreamSupervisor::new(1);
        let occupied = supervisor.root.reserve(Arc::new(227u32)).unwrap();
        assert!(matches!(supervisor.reserve(), Err(StreamReservationError::RootCapacity)));
        assert_eq!(supervisor.admission.in_use(), Ok(0));
        occupied.release();
        let ticket = supervisor.reserve().unwrap();
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = ticket.with_context::<(), ()>(|| panic!("private callback panic"));
        })).is_err());
        assert_eq!(supervisor.admission.in_use(), Ok(0));
        assert!(supervisor.reserve().is_ok());
        supervisor.root.close_admission().unwrap();
        assert!(matches!(supervisor.reserve(), Err(StreamReservationError::RootClosed)));
        assert_eq!(supervisor.admission.in_use(), Ok(0));
    }
    #[test]
    fn context_factory_error_debug_does_not_expose_private_values() {
        struct Secret;
        let error = StreamContextFactoryError::Factory(Secret);
        assert_eq!(format!("{error:?}"), "StreamContextFactoryError::Factory");
    }
    #[test]
    fn successful_ticket_cannot_allocate_a_second_context() {
        let supervisor = StreamSupervisor::new(1);
        let (ticket, original) = supervisor.reserve().unwrap().with_context(||
            OwnedStreamContext::try_new(Arc::new(229u32), ranvier_core::Bus::new())).unwrap();
        let calls = AtomicUsize::new(0);
        let result = ticket.with_context(|| {
            calls.fetch_add(1, Ordering::SeqCst);
            OwnedStreamContext::try_new(Arc::new(()), ranvier_core::Bus::new())
        });
        assert!(matches!(result, Err(StreamContextFactoryError::AlreadyCreated)));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(supervisor.admission.in_use(), Ok(0));
        assert_eq!(*original.into_parts().0, 229);
    }
    #[test]
    fn successful_factory_retains_exact_context_and_unique_ticket() {
        let supervisor = StreamSupervisor::new(1);
        let resources = Arc::new(211u32);
        let (ticket, context) = supervisor.reserve().unwrap().with_context(|| {
            let mut bus = ranvier_core::Bus::new(); bus.insert(223u32);
            OwnedStreamContext::try_new(resources.clone(), bus)
        }).unwrap();
        let (returned, bus) = context.into_parts();
        assert!(Arc::ptr_eq(&resources, &returned));
        assert_eq!(*bus.read::<u32>().unwrap(), 223);
        assert_eq!(supervisor.admission.in_use(), Ok(1));
        assert!(supervisor.reserve().is_err());
        drop(ticket);
        assert_eq!(supervisor.admission.in_use(), Ok(0));
        assert!(supervisor.reserve().is_ok());
    }
}
