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
/// Unique reservation of one recovery owner and one root record.
/// Drop refunds undecided slots. This is neither cloneable nor serializable.
pub struct StreamReservation {
    pub(crate) admission: StreamRecoveryAdmission,
    pub(crate) permit: RecoveryPermit,
    pub(crate) client: WeakCustodyClient,
    pub(crate) slot: CustodyReservation,
}
impl StreamSupervisor {
    pub fn new(capacity: usize) -> Self {
        Self { admission: StreamRecoveryAdmission::new(capacity), root: CustodyStore::new(capacity) }
    }
    /// Reserve both finite capacities before invoking request/context callbacks.
    /// Partial failure refunds the first reservation before returning.
    pub fn reserve(&self) -> Result<StreamReservation, StreamReservationError> {
        use crate::streaming_recovery_admission::RecoveryAdmissionFailure as Failure;
        let permit = self.admission.reserve().map_err(|error| match error {
            Failure::Capacity => StreamReservationError::RecoveryCapacity,
            Failure::Closed => StreamReservationError::RecoveryClosed,
            _ => StreamReservationError::RecoveryUnavailable,
        })?;
        let slot = self.root.reserve_stream_slot().map_err(|signal| match signal {
            "execution.nested_custody_capacity" => StreamReservationError::RootCapacity,
            "execution.nested_custody_admission_closed" | "execution.parent_custody_released" => StreamReservationError::RootClosed,
            _ => StreamReservationError::RootUnavailable,
        })?;
        Ok(StreamReservation { admission: self.admission.clone(), permit, client: self.root.client(), slot })
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
    /// Factory errors or panic unwind refund both reservations. Successful return
    /// retains the unique ticket alongside the exact context for later binding.
    pub fn with_context<Res, X>(self, factory: impl FnOnce() -> Result<OwnedStreamContext<Res>, X>)
        -> Result<(Self, OwnedStreamContext<Res>), X> {
        let context = factory()?;
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
        assert!(matches!(result, Err("original factory error")));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(supervisor.admission.in_use(), Ok(0));
        assert!(supervisor.reserve().is_ok());
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
