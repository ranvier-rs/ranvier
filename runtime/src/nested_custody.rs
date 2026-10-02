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
pub(crate) type Capability = Arc<dyn Any + Send + Sync>;
struct CustodyRecord {
    owner: Capability,
    task: Option<Capability>,
}
struct StoreInner {
    closed: AtomicBool,
    admission_closed: AtomicBool,
    capacity: usize,
    records: Mutex<HashMap<uuid::Uuid, CustodyRecord>>,
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
/// One pre-context root slot. No user capability lives in its placeholder.
/// Finalization is a single decision; Drop retires only an unfinalized marker.
#[cfg(feature = "streaming")]
pub(crate) struct CustodyReservation {
    receipt: Option<WeakCustody>,
    marker: Capability,
}
#[cfg(feature = "streaming")]
impl CustodyReservation {
    /// Check the original placeholder before a context callback. No callback or
    /// arbitrary resource destruction occurs while holding the registry lock.
    pub(crate) fn validate_open(&self, client: &WeakCustodyClient) -> Result<(), &'static str> {
        let receipt = self.receipt.as_ref().ok_or("execution.nested_reservation_decided")?;
        if !Weak::ptr_eq(&client.0, &receipt.store) { return Err("execution.nested_custody_mismatch"); }
        let store = receipt.store.upgrade().ok_or("execution.parent_custody_released")?;
        let records = store.records.lock().map_err(|_| "execution.nested_custody_unavailable")?;
        if store.closed.load(Ordering::Acquire) { return Err("execution.parent_custody_released"); }
        if store.admission_closed.load(Ordering::Acquire) { return Err("execution.nested_custody_admission_closed"); }
        let record = records.get(&receipt.id).ok_or("execution.parent_custody_released")?;
        if !Arc::ptr_eq(&record.owner, &self.marker) || record.task.is_some() {
            return Err("execution.nested_reservation_mismatch");
        }
        Ok(())
    }
    pub(crate) fn finalize_stream(&mut self, client: &WeakCustodyClient, owner: Capability,
        task: Capability, journal: &crate::SagaRollbackJournal,
        observer: tokio::sync::watch::Receiver<crate::streaming_recovery::SupervisorObservation>)
        -> Result<WeakCustody, &'static str> {
        let receipt = self.receipt.as_ref().ok_or("execution.nested_reservation_decided")?;
        if !Weak::ptr_eq(&client.0, &receipt.store) { return Err("execution.nested_custody_mismatch"); }
        let store = receipt.store.upgrade().ok_or("execution.parent_custody_released")?;
        let released = {
            let mut records = store.records.lock().map_err(|_| "execution.nested_custody_unavailable")?;
            if store.closed.load(Ordering::Acquire) { return Err("execution.parent_custody_released"); }
            // A closed drain snapshot must not miss a subsequently attached task.
            if store.admission_closed.load(Ordering::Acquire) { return Err("execution.nested_custody_admission_closed"); }
            let record = records.get_mut(&receipt.id).ok_or("execution.parent_custody_released")?;
            if !Arc::ptr_eq(&record.owner, &self.marker) || record.task.is_some() {
                return Err("execution.nested_reservation_mismatch");
            }
            journal.retain_stream_child(observer);
            std::mem::replace(record, CustodyRecord { owner: owner.clone(), task: Some(task) })
        };
        let registered = WeakCustody { id: receipt.id, record: Arc::downgrade(&owner), store: receipt.store.clone() };
        self.receipt = None;
        drop(released);
        Ok(registered)
    }
}
#[cfg(feature = "streaming")]
impl Drop for CustodyReservation {
    fn drop(&mut self) {
        let Some(receipt) = &self.receipt else { return; };
        let Some(store) = receipt.store.upgrade() else { return; };
        let released = {
            let mut records = store.records.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if records.get(&receipt.id).is_some_and(|record| Arc::ptr_eq(&record.owner, &self.marker) && record.task.is_none()) {
                records.remove(&receipt.id)
            } else { None }
        };
        drop(released);
    }
}

impl CustodyStore {
    #[cfg(feature = "streaming")]
    pub(crate) fn reserve_stream_slot(&self) -> Result<CustodyReservation, &'static str> {
        let marker: Capability = Arc::new(());
        let receipt = reserve_in(&self.0 .0, marker.clone())?;
        Ok(CustodyReservation { receipt: Some(receipt), marker })
    }
    pub(crate) fn new(capacity: usize) -> Self {
        Self(Arc::new(StoreOwner(Arc::new(StoreInner {
            closed: AtomicBool::new(false),
            admission_closed: AtomicBool::new(false),
            capacity,
            records: Mutex::new(HashMap::new()),
        }))))
    }
    /// Stop future admission while retaining existing recovery capabilities.
    /// Serialize with reserve; final owner Drop remains a separate retirement.
    pub(crate) fn close_admission(&self) -> Result<(), &'static str> {
        let _records = self.0 .0.records.lock()
            .map_err(|_| "execution.nested_custody_unavailable")?;
        self.0 .0.admission_closed.store(true, Ordering::Release);
        Ok(())
    }
    /// Root-only snapshot after admission closure. Cloning holders preserves
    /// unjoined task custody; this neither removes records nor proves join.
    pub(crate) fn closed_tasks(&self) -> Result<Vec<(uuid::Uuid, Capability)>, &'static str> {
        let records = self.0 .0.records.lock()
            .map_err(|_| "execution.nested_custody_unavailable")?;
        if !self.0 .0.admission_closed.load(Ordering::Acquire) {
            return Err("execution.nested_custody_admission_open");
        }
        Ok(records.iter().filter_map(|(id, record)|
            record.task.as_ref().map(|task| (*id, task.clone()))).collect())
    }
    /// Acquire result custody before waiting, so dropped waits and root/record
    /// retirement cannot discard reports while this family lease remains owned.
    #[cfg(feature = "streaming")]
    pub(crate) fn closed_typed_family<T: Send + 'static>(&self) -> Result<ManagedTaskFamily<T>, &'static str> {
        let tasks = self.closed_tasks()?.into_iter().map(|(id, holder)| {
            holder.downcast::<tokio::sync::Mutex<crate::managed_stream_task::ManagedStreamTask<T>>>()
                .map(|task| (id, task)).map_err(|_| "execution.nested_task_type_mismatch")
        }).collect::<Result<Vec<_>, _>>()?;
        Ok(ManagedTaskFamily { tasks, statuses: Vec::new() })
    }
    #[cfg(feature = "streaming")]
    pub(crate) async fn settle_closed_tasks<T: Send + 'static>(
        &self, deadline: tokio::time::Instant,
    ) -> Result<ManagedTaskFamily<T>, &'static str> {
        let mut family = self.closed_typed_family()?;
        family.settle_until(deadline).await;
        Ok(family)
    }
    pub(crate) fn client(&self) -> WeakCustodyClient {
        WeakCustodyClient(Arc::downgrade(&self.0 .0))
    }
    pub(crate) fn reserve(&self, record: Capability) -> Result<WeakCustody, &'static str> {
        reserve_in(&self.0 .0, record)
    }
}
/// Private typed family lease; no serialization or Debug of task outputs.
#[cfg(feature = "streaming")]
pub(crate) struct ManagedTaskFamily<T> {
    tasks: Vec<(uuid::Uuid, Arc<tokio::sync::Mutex<crate::managed_stream_task::ManagedStreamTask<T>>>)>,
    statuses: Vec<(uuid::Uuid, Result<(), crate::managed_stream_task::ManagedTaskWaitError>)>,
}
#[cfg(feature = "streaming")]
impl<T> std::ops::Deref for ManagedTaskFamily<T> {
    type Target = [(uuid::Uuid, Result<(), crate::managed_stream_task::ManagedTaskWaitError>)];
    fn deref(&self) -> &Self::Target { &self.statuses }
}
#[cfg(feature = "streaming")]
impl<T: Send + 'static> ManagedTaskFamily<T> {
    pub(crate) async fn settle_until(&mut self, deadline: tokio::time::Instant) {
        use futures_util::{stream::FuturesUnordered, StreamExt};
        let mut pending = FuturesUnordered::new();
        for (id, task) in &self.tasks {
            pending.push(async move {
                let result = match tokio::time::timeout_at(deadline, task.lock()).await {
                    Ok(mut task) => task.settle_until(deadline).await,
                    Err(_) => Err(crate::managed_stream_task::ManagedTaskWaitError::TimedOut),
                };
                (*id, result)
            });
        }
        self.statuses.clear();
        while let Some(result) = pending.next().await { self.statuses.push(result); }
    }
    pub(crate) async fn take_joined(&self, id: uuid::Uuid, deadline: tokio::time::Instant)
        -> Result<Option<Result<Option<T>, tokio::task::JoinError>>, &'static str> {
        let task = self.tasks.iter().find(|(candidate, _)| *candidate == id)
            .map(|(_, task)| task).ok_or("execution.nested_task_not_found")?;
        let mut task = tokio::time::timeout_at(deadline, task.lock()).await
            .map_err(|_| "execution.nested_task_transfer_timeout")?;
        Ok(task.take_joined())
    }
}
impl WeakCustodyClient {
    #[cfg(feature = "streaming")]
    pub(crate) fn reserve_stream(
        &self,
        record: Capability,
        journal: &crate::SagaRollbackJournal,
        task: Option<Capability>,
        observer: tokio::sync::watch::Receiver<crate::streaming_recovery::SupervisorObservation>,
    ) -> Result<WeakCustody, &'static str> {
        let store = self.0.upgrade().ok_or("execution.parent_custody_released")?;
        reserve_in_with(&store, record, task, || journal.retain_stream_child(observer))
    }
    pub(crate) fn validate_member(
        &self,
        admission: &WeakCustody,
        owner: &Capability,
    ) -> Result<(), &'static str> {
        if !Weak::ptr_eq(&self.0, &admission.store) {
            return Err("execution.nested_stream_custody_mismatch");
        }
        let store = self
            .0
            .upgrade()
            .ok_or("execution.parent_custody_released")?;
        let records = store
            .records
            .lock()
            .map_err(|_| "execution.nested_custody_unavailable")?;
        if store.closed.load(Ordering::Acquire)
            || !records
                .get(&admission.id)
                .is_some_and(|record| Arc::ptr_eq(&record.owner, owner))
        {
            return Err("execution.parent_custody_released");
        }
        Ok(())
    }
    pub(crate) fn reserve(&self, record: Capability) -> Result<WeakCustody, &'static str> {
        let store = self
            .0
            .upgrade()
            .ok_or("execution.parent_custody_released")?;
        reserve_in(&store, record)
    }
}
fn reserve_in(store: &Arc<StoreInner>, record: Capability) -> Result<WeakCustody, &'static str> {
    reserve_in_with(store, record, None, || {})
}
// Only fixed framework bookkeeping may run under this gate. No user callbacks,
// awaits or record retirement. Lock order: store records -> journal report/list.
fn reserve_in_with(
    store: &Arc<StoreInner>, record: Capability, task: Option<Capability>, register: impl FnOnce(),
) -> Result<WeakCustody, &'static str> {
    let mut records = store
        .records
        .lock()
        .map_err(|_| "execution.nested_custody_unavailable")?;
    if store.closed.load(Ordering::Acquire) {
        drop(records);
        return Err("execution.parent_custody_released");
    }
    if store.admission_closed.load(Ordering::Acquire) {
        drop(records);
        return Err("execution.nested_custody_admission_closed");
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
    records.insert(id, CustodyRecord { owner: record, task });
    register();
    Ok(weak)
}
impl WeakCustody {
    /// Validate the exact registered task and send its framework start gate
    /// under the retirement lock. No user future is polled under this lock.
    #[cfg(feature = "streaming")]
    pub(crate) fn grant_task_start(&self, task: &Capability, permit: &mut Option<tokio::sync::oneshot::Sender<()>>)
        -> Result<(), &'static str> {
        let store = self.store.upgrade().ok_or("execution.parent_custody_released")?;
        let records = store.records.lock().map_err(|_| "execution.nested_custody_unavailable")?;
        let record = records.get(&self.id).ok_or("execution.parent_custody_released")?;
        if store.closed.load(Ordering::Acquire) { return Err("execution.parent_custody_released"); }
        if !Weak::ptr_eq(&self.record, &Arc::downgrade(&record.owner)) {
            return Err("execution.nested_task_owner_mismatch");
        }
        if !record.task.as_ref().is_some_and(|registered| Arc::ptr_eq(registered, task)) {
            return Err("execution.nested_task_registration_mismatch");
        }
        permit.take().ok_or("execution.nested_task_start_decided")?
            .send(()).map_err(|_| "execution.nested_task_start_unavailable")
    }
    /// In-process custody correlation only; never a persisted event identity.
    #[cfg(feature = "streaming")]
    pub(crate) fn id(&self) -> uuid::Uuid { self.id }
    #[cfg(test)]
    pub(crate) fn upgrade(&self) -> Option<Capability> {
        self.record.upgrade()
    }
    pub(crate) fn release(&self) {
        if let Some(store) = self.store.upgrade() {
            let released = {
                let mut records = store.records.lock().unwrap_or_else(|p| p.into_inner());
                if records.get(&self.id).is_some_and(|record|
                    Weak::ptr_eq(&self.record, &Arc::downgrade(&record.owner))) {
                    records.remove(&self.id)
                } else { None }
            };
            // User resources/handlers may have destructors; never release under lock.
            drop(released);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "streaming")]
    fn slot_observer() -> tokio::sync::watch::Receiver<crate::streaming_recovery::SupervisorObservation> {
        tokio::sync::watch::channel(crate::streaming_recovery::SupervisorObservation {
            phase: crate::streaming_recovery::SupervisorPhase::NotStarted,
            disposition: None, cleanup_complete: None, cleanup_timed_out: false,
            custody: crate::streaming_recovery::RecoveryCustody::Held,
        }).1
    }
    #[cfg(feature = "streaming")]
    #[tokio::test]
    async fn reserved_root_slot_transfers_same_record_and_starts_exact_frame_once() {
        use crate::managed_stream_task::PreparedStreamTask;
        let root = CustodyStore::new(1);
        let mut reservation = root.reserve_stream_slot().unwrap();
        let id = reservation.receipt.as_ref().unwrap().id();
        let stale = reservation.receipt.as_ref().unwrap().clone();
        assert!(root.reserve_stream_slot().is_err());
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let owner: Capability = calls.clone();
        let observed = calls.clone();
        let mut prepared = PreparedStreamTask::new_owned(owner.clone(), async move {
            observed.fetch_add(1, Ordering::SeqCst); 191u32
        });
        let journal = crate::SagaRollbackJournal::new("reserved-root");
        let registered = reservation.finalize_stream(&root.client(), owner.clone(), prepared.holder(), &journal, slot_observer()).unwrap();
        assert_eq!(registered.id(), id);
        assert_eq!(root.0 .0.records.lock().unwrap().len(), 1);
        assert_eq!(journal.retained_stream_observations().len(), 1);
        assert!(matches!(reservation.finalize_stream(&root.client(), owner, prepared.holder(), &journal, slot_observer()),
            Err("execution.nested_reservation_decided")));
        drop(reservation);
        assert!(registered.upgrade().is_some());
        assert_eq!(prepared.grant_registered_start(&stale), Err("execution.nested_task_owner_mismatch"));
        stale.release();
        assert!(registered.upgrade().is_some());
        assert_eq!(root.0 .0.records.lock().unwrap().len(), 1);
        prepared.grant_registered_start(&registered).unwrap();
        assert!(matches!(prepared.holder().lock().await.join_until(tokio::time::Instant::now() + std::time::Duration::from_secs(5)).await, Ok(Ok(Some(191)))));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[cfg(feature = "streaming")]
    #[tokio::test]
    async fn reserved_root_slot_rejection_has_no_phantom_observer_or_effect() {
        use crate::managed_stream_task::PreparedStreamTask;
        for mode in 0..3 {
            let root = CustodyStore::new(1);
            let foreign = CustodyStore::new(1);
            let client = if mode == 0 { foreign.client() } else { root.client() };
            let mut reservation = root.reserve_stream_slot().unwrap();
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let owner: Capability = calls.clone();
            let observed = calls.clone();
            let mut prepared = PreparedStreamTask::new_owned(owner.clone(), async move {
                observed.fetch_add(1, Ordering::SeqCst); 193u32
            });
            let journal = crate::SagaRollbackJournal::new("rejected-root-slot");
            if mode == 1 { root.close_admission().unwrap(); }
            let root = if mode == 2 { drop(root); None } else { Some(root) };
            let expected = match mode {
                0 => "execution.nested_custody_mismatch",
                1 => "execution.nested_custody_admission_closed",
                _ => "execution.parent_custody_released",
            };
            assert!(matches!(reservation.finalize_stream(&client, owner, prepared.holder(), &journal, slot_observer()), Err(error) if error == expected));
            assert!(journal.retained_stream_observations().is_empty());
            drop(reservation);
            if let Some(root) = root {
                assert!(root.0 .0.records.lock().unwrap().is_empty());
                if mode == 0 { assert!(root.reserve_stream_slot().is_ok()); }
                else { assert!(root.reserve_stream_slot().is_err()); }
            }
            assert!(matches!(prepared.reject_until(tokio::time::Instant::now() + std::time::Duration::from_secs(5)).await, Ok(Ok(None))));
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        }
    }

    #[cfg(feature = "streaming")]
    #[tokio::test]
    async fn ordinary_owner_receipt_cannot_grant_prepared_task() {
        use crate::managed_stream_task::PreparedStreamTask;
        use tokio::time::Instant;
        let root = CustodyStore::new(1);
        let ordinary = root.reserve(Arc::new(127u32)).unwrap();
        root.close_admission().unwrap();
        let mut prepared = PreparedStreamTask::new(async { panic!("ordinary owner must not authorize start"); });
        assert_eq!(prepared.grant_registered_start(&ordinary), Err("execution.nested_task_registration_mismatch"));
        assert!(matches!(prepared.reject_until(Instant::now() + std::time::Duration::from_secs(5)).await, Ok(Ok(None))));
        assert!(ordinary.upgrade().is_some());
    }
    #[cfg(feature = "streaming")]
    #[tokio::test]
    async fn poisoned_registered_start_keeps_gate_rejectable_and_joinable() {
        use crate::managed_stream_task::PreparedStreamTask;
        use tokio::time::Instant;
        let root = CustodyStore::new(1);
        let effects = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = effects.clone();
        let mut prepared = PreparedStreamTask::new(async move { observed.fetch_add(1, Ordering::SeqCst); });
        let admission = reserve_in_with(&root.0 .0, Arc::new(()), Some(prepared.holder()), || {}).unwrap();
        let core = root.0 .0.clone();
        assert!(std::thread::spawn(move || {
            let _held = core.records.lock().unwrap();
            panic!("fixture poison");
        }).join().is_err());
        assert_eq!(prepared.grant_registered_start(&admission), Err("execution.nested_custody_unavailable"));
        assert!(matches!(prepared.reject_until(Instant::now() + std::time::Duration::from_secs(5)).await, Ok(Ok(None))));
        assert_eq!(effects.load(Ordering::SeqCst), 0);
        drop(root);
        assert_eq!(prepared.grant_registered_start(&admission), Err("execution.parent_custody_released"));
    }
    #[cfg(feature = "streaming")]
    #[tokio::test]
    async fn registered_start_rejects_other_holder_and_retired_record_without_consuming_gate() {
        use crate::managed_stream_task::PreparedStreamTask;
        use tokio::time::Instant;
        let root = CustodyStore::new(2);
        let effects = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = effects.clone();
        let mut first = PreparedStreamTask::new(async move { observed.fetch_add(1, Ordering::SeqCst); });
        let mut second = PreparedStreamTask::new(async {});
        let first_record = reserve_in_with(&root.0 .0, Arc::new(()), Some(first.holder()), || {}).unwrap();
        let second_record = reserve_in_with(&root.0 .0, Arc::new(()), Some(second.holder()), || {}).unwrap();
        root.close_admission().unwrap();
        assert_eq!(first.grant_registered_start(&second_record), Err("execution.nested_task_registration_mismatch"));
        assert_eq!(effects.load(Ordering::SeqCst), 0);
        first.grant_registered_start(&first_record).unwrap();
        assert!(matches!(first.holder().lock().await.join_until(Instant::now() + std::time::Duration::from_secs(5)).await, Ok(Ok(Some(())))));
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        second_record.release();
        assert_eq!(second.grant_registered_start(&second_record), Err("execution.parent_custody_released"));
        assert!(matches!(second.reject_until(Instant::now() + std::time::Duration::from_secs(5)).await, Ok(Ok(None))));
    }
    #[cfg(feature = "streaming")]
    #[tokio::test(start_paused = true)]
    async fn family_transfer_deadline_and_unknown_id_preserve_original_result() {
        use crate::managed_stream_task::ManagedStreamTask;
        use tokio::{sync::Mutex as AsyncMutex, time::Instant};
        let root = CustodyStore::new(1);
        let (task, permit) = ManagedStreamTask::prepare(async { 103u32 });
        let task = Arc::new(AsyncMutex::new(task));
        let admission = reserve_in_with(&root.0 .0, Arc::new(()), Some(task.clone()), || {}).unwrap();
        root.close_admission().unwrap();
        permit.send(()).unwrap();
        let family = root.settle_closed_tasks::<u32>(Instant::now() + std::time::Duration::from_secs(5)).await.unwrap();
        assert!(matches!(family.take_joined(uuid::Uuid::new_v4(), Instant::now()).await, Err("execution.nested_task_not_found")));
        let held = task.lock().await;
        assert!(matches!(family.take_joined(admission.id, Instant::now() + std::time::Duration::from_secs(3)).await, Err("execution.nested_task_transfer_timeout")));
        drop(held);
        assert_eq!(family.take_joined(admission.id, Instant::now()).await.unwrap().unwrap().unwrap(), Some(103));
    }
    #[cfg(feature = "streaming")]
    #[tokio::test]
    async fn family_lease_preserves_report_after_root_loss_and_dropped_wait() {
        use crate::managed_stream_task::ManagedStreamTask;
        use tokio::{sync::{Mutex as AsyncMutex, oneshot}, time::Instant};
        let root = CustodyStore::new(1);
        let (finish, gate) = oneshot::channel();
        let (task, permit) = ManagedStreamTask::prepare(async move { Box::new(gate.await.unwrap()) });
        let task = Arc::new(AsyncMutex::new(task));
        let admission = reserve_in_with(&root.0 .0, Arc::new(()), Some(task), || {}).unwrap();
        root.close_admission().unwrap();
        let mut family = root.closed_typed_family::<Box<u32>>().unwrap();
        permit.send(()).unwrap();
        {
            let wait = family.settle_until(Instant::now() + std::time::Duration::from_secs(5));
            tokio::pin!(wait);
            assert!(futures_util::poll!(&mut wait).is_pending());
        }
        admission.release();
        drop(root);
        finish.send(101u32).unwrap();
        family.settle_until(Instant::now() + std::time::Duration::from_secs(5)).await;
        assert_eq!(family.statuses, vec![(admission.id, Ok(()))]);
        assert_eq!(*family.take_joined(admission.id, Instant::now()).await.unwrap().unwrap().unwrap().unwrap(), 101);
        assert!(family.take_joined(admission.id, Instant::now()).await.unwrap().is_none());
    }
    #[cfg(feature = "streaming")]
    #[tokio::test]
    async fn concurrent_family_settlement_transfers_original_report_once() {
        use crate::managed_stream_task::ManagedStreamTask;
        use tokio::{sync::Mutex as AsyncMutex, time::Instant};
        let root = CustodyStore::new(1);
        let (task, permit) = ManagedStreamTask::prepare(async { Box::new(89u32) });
        let task = Arc::new(AsyncMutex::new(task));
        let admission = reserve_in_with(&root.0 .0, Arc::new(()), Some(task.clone()), || {}).unwrap();
        root.close_admission().unwrap();
        permit.send(()).unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        let (first, second) = tokio::join!(root.settle_closed_tasks::<Box<u32>>(deadline), root.settle_closed_tasks::<Box<u32>>(deadline));
        assert_eq!(first.unwrap().statuses, vec![(admission.id, Ok(()))]);
        assert_eq!(second.unwrap().statuses, vec![(admission.id, Ok(()))]);
        let (first, second) = tokio::join!(
            async { task.lock().await.take_joined() },
            async { task.lock().await.take_joined() },
        );
        assert_eq!(usize::from(first.is_some()) + usize::from(second.is_some()), 1);
        assert_eq!(*first.or(second).unwrap().unwrap().unwrap(), 89);
    }

    #[cfg(feature = "streaming")]
    #[tokio::test]
    async fn active_family_snapshot_survives_record_retirement() {
        use crate::managed_stream_task::ManagedStreamTask;
        use tokio::{sync::{Mutex as AsyncMutex, oneshot}, time::Instant};
        let root = CustodyStore::new(1);
        let (finish, gate) = oneshot::channel();
        let (task, permit) = ManagedStreamTask::prepare(async move { gate.await.unwrap() });
        let task = Arc::new(AsyncMutex::new(task));
        let weak_task = Arc::downgrade(&task);
        let admission = reserve_in_with(&root.0 .0, Arc::new(()), Some(task.clone()), || {}).unwrap();
        root.close_admission().unwrap();
        permit.send(()).unwrap();
        drop(task);
        let wait = root.settle_closed_tasks::<u32>(Instant::now() + std::time::Duration::from_secs(5));
        tokio::pin!(wait);
        assert!(futures_util::poll!(&mut wait).is_pending());
        admission.release();
        assert!(root.closed_tasks().unwrap().is_empty());
        // Acquire explicit result custody before the snapshot can retire.
        let retained = weak_task.upgrade().unwrap();
        finish.send(97u32).unwrap();
        assert_eq!(wait.await.unwrap().statuses, vec![(admission.id, Ok(()))]);
        assert_eq!(retained.lock().await.take_joined().unwrap().unwrap(), Some(97));
        drop(retained);
        assert!(weak_task.upgrade().is_none());
    }
    #[cfg(feature = "streaming")]
    #[tokio::test(start_paused = true)]
    async fn family_lock_deadline_preserves_same_holder_for_retry() {
        use crate::managed_stream_task::{ManagedStreamTask, ManagedTaskWaitError};
        use tokio::{sync::Mutex as AsyncMutex, time::Instant};
        let root = CustodyStore::new(1);
        let (task, permit) = ManagedStreamTask::prepare(async { 83u32 });
        let task = Arc::new(AsyncMutex::new(task));
        let admission = reserve_in_with(&root.0 .0, Arc::new(()), Some(task.clone()), || {}).unwrap();
        root.close_admission().unwrap();
        permit.send(()).unwrap();
        let held = task.lock().await;
        let result = root.settle_closed_tasks::<u32>(Instant::now() + std::time::Duration::from_secs(3)).await.unwrap();
        assert_eq!(result.statuses, vec![(admission.id, Err(ManagedTaskWaitError::TimedOut))]);
        drop(held);
        let result = root.settle_closed_tasks::<u32>(Instant::now() + std::time::Duration::from_secs(3)).await.unwrap();
        assert_eq!(result.statuses, vec![(admission.id, Ok(()))]);
        assert_eq!(task.lock().await.take_joined().unwrap().unwrap(), Some(83));
    }
    #[cfg(feature = "streaming")]
    #[tokio::test]
    async fn family_settlement_drop_preserves_completed_report_and_pending_handle() {
        use crate::managed_stream_task::ManagedStreamTask;
        use tokio::{sync::{Mutex as AsyncMutex, oneshot}, time::Instant};
        let root = CustodyStore::new(2);
        let report = Arc::new(67u32);
        let weak_report = Arc::downgrade(&report);
        let (ready, ready_gate) = oneshot::channel();
        let (first, first_permit) = ManagedStreamTask::prepare(async move {
            ready.send(()).unwrap();
            report
        });
        let (finish, finish_gate) = oneshot::channel();
        let (second, second_permit) = ManagedStreamTask::prepare(async move {
            Arc::new(finish_gate.await.unwrap())
        });
        let first = Arc::new(AsyncMutex::new(first));
        let second = Arc::new(AsyncMutex::new(second));
        let first_record = reserve_in_with(&root.0 .0, Arc::new(()), Some(first.clone()), || {}).unwrap();
        let second_record = reserve_in_with(&root.0 .0, Arc::new(()), Some(second.clone()), || {}).unwrap();
        root.close_admission().unwrap();
        first_permit.send(()).unwrap();
        second_permit.send(()).unwrap();
        ready_gate.await.unwrap();
        {
            let wait = root.settle_closed_tasks::<Arc<u32>>(Instant::now() + std::time::Duration::from_secs(5));
            tokio::pin!(wait);
            assert!(futures_util::poll!(&mut wait).is_pending());
        }
        assert!(weak_report.upgrade().is_some());
        // Completed output stays in the actual holder after the family future drops.
        let original = first.lock().await.take_joined().unwrap().unwrap().unwrap();
        assert_eq!(*original, 67);
        finish.send(71u32).unwrap();
        let settled = root.settle_closed_tasks::<Arc<u32>>(Instant::now() + std::time::Duration::from_secs(5)).await.unwrap();
        assert_eq!(settled.len(), 2);
        assert!(settled.iter().any(|(id, status)| *id == first_record.id && *status == Err(crate::managed_stream_task::ManagedTaskWaitError::AlreadyJoined)));
        assert!(settled.iter().any(|(id, status)| *id == second_record.id && *status == Ok(())));
        assert_eq!(*second.lock().await.take_joined().unwrap().unwrap().unwrap(), 71);
        drop(original);
        assert!(weak_report.upgrade().is_none());
    }

    #[cfg(feature = "streaming")]
    #[tokio::test]
    async fn family_type_preflight_does_not_consume_matching_task() {
        use crate::managed_stream_task::ManagedStreamTask;
        use tokio::{sync::Mutex as AsyncMutex, time::Instant};
        let root = CustodyStore::new(2);
        let (first, permit) = ManagedStreamTask::prepare(async { 73u32 });
        let first = Arc::new(AsyncMutex::new(first));
        reserve_in_with(&root.0 .0, Arc::new(()), Some(first.clone()), || {}).unwrap();
        let mismatch = reserve_in_with(&root.0 .0, Arc::new(()), Some(Arc::new(79u64)), || {}).unwrap();
        root.close_admission().unwrap();
        permit.send(()).unwrap();
        assert!(matches!(root.settle_closed_tasks::<u32>(Instant::now() + std::time::Duration::from_secs(5)).await, Err("execution.nested_task_type_mismatch")));
        assert!(first.lock().await.take_joined().is_none());
        mismatch.release();
        assert!(root.settle_closed_tasks::<u32>(Instant::now() + std::time::Duration::from_secs(5)).await.unwrap().iter().all(|(_, result)| result.is_ok()));
        assert_eq!(first.lock().await.take_joined().unwrap().unwrap(), Some(73));
    }
    #[test]
    fn closed_task_snapshot_excludes_plain_owners_and_keeps_holder_after_record_release() {
        let root = CustodyStore::new(2);
        let ordinary = root.reserve(Arc::new(53u32)).unwrap();
        let holder: Capability = Arc::new(59u32);
        let weak = Arc::downgrade(&holder);
        let managed = reserve_in_with(&root.0 .0, Arc::new(61u32), Some(holder), || {}).unwrap();
        root.close_admission().unwrap();
        let snapshot = root.closed_tasks().unwrap();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].0, managed.id);
        managed.release();
        ordinary.release();
        assert!(root.closed_tasks().unwrap().is_empty());
        assert!(weak.upgrade().is_some());
        drop(snapshot);
        assert!(weak.upgrade().is_none());
    }
    #[test]
    fn admission_closure_cannot_pass_framework_registration_gate() {
        let root = CustodyStore::new(1);
        let core = root.0 .0.clone();
        let registered = Arc::new(AtomicBool::new(false));
        let recorded = registered.clone();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let reserve = std::thread::spawn(move || reserve_in_with(&core, Arc::new(47u32), None, || {
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            recorded.store(true, Ordering::Release);
        }));
        entered_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert!(root.0 .0.records.try_lock().is_err());
        let closer = root.clone();
        let closed_registered = registered.clone();
        let close = std::thread::spawn(move || {
            closer.close_admission().unwrap();
            assert!(closed_registered.load(Ordering::Acquire));
        });
        release_tx.send(()).unwrap();
        let admission = reserve.join().unwrap().unwrap();
        close.join().unwrap();
        assert!(admission.upgrade().is_some());
        assert!(matches!(root.reserve(Arc::new(())), Err("execution.nested_custody_admission_closed")));
        admission.release();
    }
    #[test]
    fn concurrent_admission_close_retains_every_success_and_rejects_later_reservations() {
        let root = CustodyStore::new(17);
        let anchor: Capability = Arc::new(99u32);
        let anchor_admission = root.reserve(anchor.clone()).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(17));
        let mut workers = Vec::new();
        for value in 0u32..16 {
            let client = root.client();
            let start = barrier.clone();
            workers.push(std::thread::spawn(move || {
                let record: Capability = Arc::new(value);
                start.wait();
                client.reserve(record.clone()).map(|admission| (admission, record))
            }));
        }
        barrier.wait();
        root.close_admission().unwrap();
        let mut accepted = Vec::new();
        for worker in workers {
            match worker.join().unwrap() {
                Ok(entry) => accepted.push(entry),
                Err(signal) => assert_eq!(signal, "execution.nested_custody_admission_closed"),
            }
        }
        assert_eq!(root.0 .0.records.lock().unwrap().len(), accepted.len() + 1);
        assert!(root.client().validate_member(&anchor_admission, &anchor).is_ok());
        for (admission, record) in &accepted {
            assert!(root.client().validate_member(admission, record).is_ok());
        }
        assert!(matches!(root.reserve(Arc::new(())), Err("execution.nested_custody_admission_closed")));
        for (admission, _) in accepted { admission.release(); }
        anchor_admission.release();
        assert!(root.0 .0.records.lock().unwrap().is_empty());
    }
    #[test]
    fn admission_close_fails_closed_on_poison_without_retiring_records() {
        let root = CustodyStore::new(1);
        let admission = root.reserve(Arc::new(43u32)).unwrap();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _held = root.0 .0.records.lock().unwrap();
            panic!("fixture poison");
        }));
        assert_eq!(root.close_admission(), Err("execution.nested_custody_unavailable"));
        assert!(admission.upgrade().is_some());
        drop(root);
        assert!(admission.upgrade().is_none());
    }
    #[test]
    fn admission_close_preserves_members_until_actual_owner_retirement() {
        let root = CustodyStore::new(2);
        let client = root.client();
        let record: Capability = Arc::new(41u32);
        let weak = Arc::downgrade(&record);
        let admission = client.reserve(record.clone()).unwrap();
        root.close_admission().unwrap();
        root.close_admission().unwrap();
        assert!(matches!(client.reserve(Arc::new(())), Err("execution.nested_custody_admission_closed")));
        assert!(client.validate_member(&admission, &record).is_ok());
        drop(record);
        assert!(weak.upgrade().is_some());
        drop(root);
        assert!(weak.upgrade().is_none());
        assert!(matches!(client.reserve(Arc::new(())), Err("execution.parent_custody_released")));
    }
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
