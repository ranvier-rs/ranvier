//! Private local custody. No serialization, Debug, or replay authority.
use std::{
    any::Any,
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};

pub(crate) struct RequiredOwnedCustody;
pub(crate) struct OwnedResources<Res>(pub(crate) Arc<Res>);
pub(crate) struct NestedSagaCustody<E, Res> {
    pub(crate) resources: Arc<Res>,
    pub(crate) handlers: ranvier_core::saga::SagaCompensationRegistry<E, Res>,
}
type Capability = Arc<dyn Any + Send + Sync>;
struct StoreInner {
    capacity: usize,
    records: Mutex<HashMap<uuid::Uuid, Capability>>,
}
#[derive(Clone)]
pub(crate) struct CustodyStore(Arc<StoreInner>);
#[derive(Clone)]
pub(crate) struct WeakCustody {
    id: uuid::Uuid,
    record: Weak<dyn Any + Send + Sync>,
    store: Weak<StoreInner>,
}
impl CustodyStore {
    pub(crate) fn new(capacity: usize) -> Self {
        Self(Arc::new(StoreInner {
            capacity,
            records: Mutex::new(HashMap::new()),
        }))
    }
    pub(crate) fn reserve(&self, record: Capability) -> Result<WeakCustody, &'static str> {
        let mut records = self
            .0
            .records
            .lock()
            .map_err(|_| "execution.nested_custody_unavailable")?;
        if records.len() >= self.0.capacity {
            drop(records);
            return Err("execution.nested_custody_capacity");
        }
        let id = uuid::Uuid::new_v4();
        let weak = WeakCustody {
            id,
            record: Arc::downgrade(&record),
            store: Arc::downgrade(&self.0),
        };
        records.insert(id, record);
        Ok(weak)
    }
}
impl WeakCustody {
    #[cfg(test)]
    pub(crate) fn upgrade(&self) -> Option<Capability> {
        self.record.upgrade()
    }
    pub(crate) fn release(&self) {
        if let Some(store) = self.store.upgrade() {
            let released = store
                .records
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&self.id);
            // User resources/handlers may have destructors; never release under lock.
            drop(released);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Probe {
        store: Weak<StoreInner>,
        released: Arc<std::sync::atomic::AtomicBool>,
    }
    impl Drop for Probe {
        fn drop(&mut self) {
            if let Some(store) = self.store.upgrade() {
                assert!(store.records.try_lock().is_ok());
            }
            self.released
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    #[test]
    fn capacity_and_release_drop_user_capabilities_outside_locks() {
        let store = CustodyStore::new(1);
        let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let retained = store
            .reserve(Arc::new(Probe {
                store: Arc::downgrade(&store.0),
                released: released.clone(),
            }))
            .unwrap_or_else(|_| panic!("admission failed"));
        let refused = Arc::new(std::sync::atomic::AtomicBool::new(false));
        assert!(store
            .reserve(Arc::new(Probe {
                store: Arc::downgrade(&store.0),
                released: refused.clone()
            }))
            .is_err());
        assert!(refused.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!released.load(std::sync::atomic::Ordering::SeqCst));
        retained.release();
        assert!(released.load(std::sync::atomic::Ordering::SeqCst));
        assert!(retained.upgrade().is_none());
        assert!(store.reserve(Arc::new(())).is_ok());
    }
}
