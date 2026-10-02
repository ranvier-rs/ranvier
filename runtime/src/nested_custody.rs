//! Private local custody. No serialization, Debug, or replay authority.
use std::{
    any::Any,
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};

pub(crate) struct RequiredOwnedCustody;
pub(crate) struct OwnedResources<Res>(pub(crate) Arc<Res>);
impl<Res> Clone for OwnedResources<Res> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
/// Temporary explicit typed owner; restore the parent's original Bus storage on
/// return, panic or future Drop. Required supervision retains the live record.
pub(crate) struct OwnedResourceScope<'a> {
    bus: &'a mut ranvier_core::Bus,
    scope: Option<ranvier_core::bus::BusResourceScope>,
    pub(crate) eligible: bool,
}
impl<'a> OwnedResourceScope<'a> {
    pub(crate) fn begin<Res: Send + Sync + 'static>(
        bus: &'a mut ranvier_core::Bus,
        resources: Arc<Res>,
    ) -> Self {
        let access = bus.take_access_scope();
        let eligible = bus.has::<RequiredOwnedCustody>()
            && bus.has::<CustodyStore>()
            && bus.has::<crate::execution_observation::ActiveObservation>();
        let scope = bus.begin_resource_scope::<OwnedResources<Res>>();
        bus.insert(OwnedResources(resources));
        bus.restore_access_scope(access);
        Self {
            bus,
            scope: Some(scope),
            eligible,
        }
    }
    pub(crate) fn bus(&mut self) -> &mut ranvier_core::Bus {
        self.bus
    }
}
impl Drop for OwnedResourceScope<'_> {
    fn drop(&mut self) {
        let access = self.bus.take_access_scope();
        if let Some(scope) = self.scope.take() {
            self.bus.restore_resource_scope(scope);
        }
        self.bus.restore_access_scope(access);
    }
}
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
