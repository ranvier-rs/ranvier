//! Finite recovery-owner admission. This bounds owners, not retained payload bytes.
use std::sync::{Arc, Mutex};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveryAdmissionFailure {
    Closed,
    Capacity,
    Poisoned,
    RegistryPoisoned,
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
