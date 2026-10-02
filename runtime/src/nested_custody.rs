//! Private local custody. No serialization, Debug, or replay authority.
use std::{
    any::Any,
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, Weak,
    },
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
            && (bus.has::<CustodyStore>() || bus.has::<WeakCustodyClient>())
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
    closed: AtomicBool,
    capacity: usize,
    records: Mutex<HashMap<uuid::Uuid, Capability>>,
}
struct StoreOwner(Arc<StoreInner>);
impl Drop for StoreOwner {
    fn drop(&mut self) {
        let released = {
            let mut records = self.0.records.lock().unwrap_or_else(|p| p.into_inner());
            self.0.closed.store(true, Ordering::Release);
            std::mem::take(&mut *records)
        };
        // Closing root ownership retires records even if a client operation has
        // temporarily upgraded the core. Never release user objects under lock.
        drop(released);
    }
}
#[derive(Clone)]
pub(crate) struct CustodyStore(Arc<StoreOwner>);
#[derive(Clone)]
pub(crate) struct WeakCustodyClient(Weak<StoreInner>);
#[derive(Clone)]
pub(crate) struct WeakCustody {
    id: uuid::Uuid,
    record: Weak<dyn Any + Send + Sync>,
    store: Weak<StoreInner>,
}
impl CustodyStore {
    pub(crate) fn new(capacity: usize) -> Self {
        Self(Arc::new(StoreOwner(Arc::new(StoreInner {
            closed: AtomicBool::new(false),
            capacity,
            records: Mutex::new(HashMap::new()),
        }))))
    }
    pub(crate) fn client(&self) -> WeakCustodyClient {
        WeakCustodyClient(Arc::downgrade(&self.0 .0))
    }
    pub(crate) fn reserve(&self, record: Capability) -> Result<WeakCustody, &'static str> {
        reserve_in(&self.0 .0, record)
    }
}
impl WeakCustodyClient {
    pub(crate) fn reserve(&self, record: Capability) -> Result<WeakCustody, &'static str> {
        let store = self
            .0
            .upgrade()
            .ok_or("execution.parent_custody_released")?;
        reserve_in(&store, record)
    }
}
fn reserve_in(store: &Arc<StoreInner>, record: Capability) -> Result<WeakCustody, &'static str> {
    let mut records = store
        .records
        .lock()
        .map_err(|_| "execution.nested_custody_unavailable")?;
    if store.closed.load(Ordering::Acquire) {
        drop(records);
        return Err("execution.parent_custody_released");
    }
    if records.len() >= store.capacity {
        drop(records);
        return Err("execution.nested_custody_capacity");
    }
    let id = uuid::Uuid::new_v4();
    let weak = WeakCustody {
        id,
        record: Arc::downgrade(&record),
        store: Arc::downgrade(store),
    };
    records.insert(id, record);
    Ok(weak)
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
    #[test]
    fn resource_scope_restores_exact_owned_shared_aliased_and_inherited_storage() {
        struct NonClone(u32);
        for role in 0..4 {
            let original = Arc::new(NonClone(41));
            let replacement = Arc::new(NonClone(42));
            let mut bus = ranvier_core::Bus::new();
            if role == 0 {
                bus.insert(OwnedResources(original.clone()));
            } else {
                bus.insert_shared(OwnedResources(original.clone()));
            }
            let sibling = if role == 2 {
                Some(bus.fork_for_parallel())
            } else {
                None
            };
            let source = if role == 3 {
                Some(std::mem::replace(&mut bus, ranvier_core::Bus::new()))
            } else {
                None
            };
            if let Some(source) = &source {
                bus = source.fork_for_parallel();
            }
            let address = bus.read::<OwnedResources<NonClone>>().unwrap() as *const _ as usize;
            let writable = bus.read_mut::<OwnedResources<NonClone>>().is_some();
            bus.set_access_policy(
                "deny-resource-scope",
                Some(ranvier_core::BusAccessPolicy::allow_only(Vec::new())),
            );
            {
                let mut scope = OwnedResourceScope::begin(&mut bus, replacement.clone());
                let access = scope.bus().take_access_scope();
                let selected = scope.bus().read::<OwnedResources<NonClone>>().unwrap();
                assert!(Arc::ptr_eq(&selected.0, &replacement));
                assert_eq!(selected.0 .0, 42);
                scope.bus().restore_access_scope(access);
            }
            assert!(bus.get::<OwnedResources<NonClone>>().is_err());
            let access = bus.take_access_scope();
            assert_eq!(
                bus.read::<OwnedResources<NonClone>>().unwrap() as *const _ as usize,
                address
            );
            assert!(Arc::ptr_eq(
                &bus.read::<OwnedResources<NonClone>>().unwrap().0,
                &original
            ));
            assert_eq!(
                bus.read_mut::<OwnedResources<NonClone>>().is_some(),
                writable
            );
            bus.restore_access_scope(access);
            drop(sibling);
        }
    }
    #[test]
    fn weak_clients_share_capacity_and_cannot_keep_root_records_alive() {
        struct Child {
            _client: WeakCustodyClient,
        }
        let root = CustodyStore::new(1);
        let client = root.client();
        let child = Arc::new(Child {
            _client: client.clone(),
        });
        let weak_child = Arc::downgrade(&child);
        let record = client
            .reserve(child)
            .unwrap_or_else(|_| panic!("client admission failed"));
        assert!(matches!(
            root.reserve(Arc::new(())),
            Err("execution.nested_custody_capacity")
        ));
        assert!(matches!(
            client.reserve(Arc::new(())),
            Err("execution.nested_custody_capacity")
        ));
        let temporary_core = client.0.upgrade().unwrap();
        drop(root);
        assert!(weak_child.upgrade().is_none());
        assert!(record.upgrade().is_none());
        assert!(matches!(
            client.reserve(Arc::new(())),
            Err("execution.parent_custody_released")
        ));
        assert!(temporary_core.closed.load(Ordering::Acquire));
    }
    #[test]
    fn concurrent_weak_clients_use_one_global_capacity_and_last_root_clone_closes_it() {
        let root = CustodyStore::new(4);
        let client = root.client();
        let barrier = Arc::new(std::sync::Barrier::new(17));
        let threads: Vec<_> = (0..16)
            .map(|index| {
                let client = client.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    client.reserve(Arc::new(index))
                })
            })
            .collect();
        barrier.wait();
        let mut admitted = Vec::new();
        for thread in threads {
            match thread.join().unwrap() {
                Ok(record) => admitted.push(record),
                Err(signal) => assert_eq!(signal, "execution.nested_custody_capacity"),
            }
        }
        assert_eq!(admitted.len(), 4);
        let remaining_root = root.clone();
        drop(root);
        assert!(admitted.iter().all(|record| record.upgrade().is_some()));
        drop(remaining_root);
        assert!(admitted.iter().all(|record| record.upgrade().is_none()));
        assert!(matches!(
            client.reserve(Arc::new(())),
            Err("execution.parent_custody_released")
        ));
    }
    #[test]
    fn weak_client_poison_is_unavailable_and_root_close_retires_records() {
        let root = CustodyStore::new(1);
        let client = root.client();
        let record = root
            .reserve(Arc::new(()))
            .unwrap_or_else(|_| panic!("admission failed"));
        let core = root.0 .0.clone();
        assert!(std::thread::spawn(move || {
            let _held = core.records.lock().unwrap();
            panic!("fixture poison");
        })
        .join()
        .is_err());
        assert!(matches!(
            client.reserve(Arc::new(())),
            Err("execution.nested_custody_unavailable")
        ));
        drop(root);
        assert!(record.upgrade().is_none());
    }
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
                store: Arc::downgrade(&store.0 .0),
                released: released.clone(),
            }))
            .unwrap_or_else(|_| panic!("admission failed"));
        let refused = Arc::new(std::sync::atomic::AtomicBool::new(false));
        assert!(store
            .reserve(Arc::new(Probe {
                store: Arc::downgrade(&store.0 .0),
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
