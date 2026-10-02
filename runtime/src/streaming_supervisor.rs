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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
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
