//! Finite recovery-owner admission. This bounds owners, not retained payload bytes.
use std::sync::{Arc, Mutex};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveryAdmissionFailure {
    Closed,
    Capacity,
    Poisoned,
    RegistryPoisoned,
    RegistryBusy,
    InvalidRetention,
}
#[derive(Clone)]
pub(crate) struct StreamRecoveryAdmission(Arc<Mutex<State>>);
struct State {
    closed: bool,
    capacity: usize,
    used: usize,
}
pub(crate) struct RecoveryPermit(Arc<Mutex<State>>);
impl StreamRecoveryAdmission {
    pub(crate) fn new(capacity: usize) -> Self {
        Self(Arc::new(Mutex::new(State {
            closed: false,
            capacity,
            used: 0,
        })))
    }
    pub(crate) fn reserve(&self) -> Result<RecoveryPermit, RecoveryAdmissionFailure> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| RecoveryAdmissionFailure::Poisoned)?;
        if state.closed {
            return Err(RecoveryAdmissionFailure::Closed);
        }
        if state.used >= state.capacity {
            return Err(RecoveryAdmissionFailure::Capacity);
        }
        state.used += 1;
        Ok(RecoveryPermit(self.0.clone()))
    }
    pub(crate) fn close(&self) -> Result<(), RecoveryAdmissionFailure> {
        self.0
            .lock()
            .map_err(|_| RecoveryAdmissionFailure::Poisoned)?
            .closed = true;
        Ok(())
    }
    pub(crate) fn in_use(&self) -> Result<usize, RecoveryAdmissionFailure> {
        Ok(self
            .0
            .lock()
            .map_err(|_| RecoveryAdmissionFailure::Poisoned)?
            .used)
    }
}
impl Drop for RecoveryPermit {
    fn drop(&mut self) {
        // Release only, never reopen/admit from poisoned state.
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.used > 0 {
            state.used -= 1;
        } else {
            tracing::error!("recovery reservation release without active permit");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn simultaneous_reservations_stay_bounded_and_close_cannot_reopen() {
        let admission = StreamRecoveryAdmission::new(4);
        let ready = Arc::new(std::sync::Barrier::new(17));
        let release = Arc::new(std::sync::Barrier::new(17));
        let workers: Vec<_> = (0..16)
            .map(|_| {
                let admission = admission.clone();
                let ready = ready.clone();
                let release = release.clone();
                std::thread::spawn(move || {
                    let permit = admission.reserve().ok();
                    let accepted = permit.is_some();
                    ready.wait();
                    release.wait();
                    drop(permit);
                    accepted
                })
            })
            .collect();
        ready.wait();
        assert_eq!(admission.in_use(), Ok(4));
        admission.close().unwrap();
        assert!(matches!(
            admission.reserve(),
            Err(RecoveryAdmissionFailure::Closed)
        ));
        release.wait();
        assert_eq!(
            workers
                .into_iter()
                .map(|worker| usize::from(worker.join().unwrap()))
                .sum::<usize>(),
            4
        );
        assert_eq!(admission.in_use(), Ok(0));
        assert!(matches!(
            admission.reserve(),
            Err(RecoveryAdmissionFailure::Closed)
        ));
    }
    #[test]
    fn zero_and_poisoned_admission_fail_closed() {
        assert!(matches!(
            StreamRecoveryAdmission::new(0).reserve(),
            Err(RecoveryAdmissionFailure::Capacity)
        ));
        let admission = StreamRecoveryAdmission::new(1);
        let permit = admission.reserve().unwrap();
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _state = admission.0.lock().unwrap();
            panic!("fixture admission poison");
        }));
        assert!(poisoned.is_err());
        assert!(matches!(
            admission.reserve(),
            Err(RecoveryAdmissionFailure::Poisoned)
        ));
        assert_eq!(admission.close(), Err(RecoveryAdmissionFailure::Poisoned));
        drop(permit);
        assert_eq!(admission.in_use(), Err(RecoveryAdmissionFailure::Poisoned));
        assert!(matches!(
            admission.reserve(),
            Err(RecoveryAdmissionFailure::Poisoned)
        ));
    }
}
