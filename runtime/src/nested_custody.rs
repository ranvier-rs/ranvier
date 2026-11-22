//! Private local custody. No serialization, Debug, or replay authority.
use std::{
    any::Any,
    collections::HashMap,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
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
///
/// Used by the public `Axon::execute_owned_child*` entry points.
#[cfg(feature = "streaming")]
pub(crate) struct OwnedResourceScope<'a> {
    bus: &'a mut ranvier_core::Bus,
    scope: Option<ranvier_core::bus::BusResourceScope>,
    pub(crate) eligible: bool,
}
#[cfg(feature = "streaming")]
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
#[cfg(feature = "streaming")]
impl Drop for OwnedResourceScope<'_> {
    fn drop(&mut self) {
        let access = self.bus.take_access_scope();
        if let Some(scope) = self.scope.take() {
            self.bus.restore_resource_scope(scope);
        }
        self.bus.restore_access_scope(access);
    }
}
/// Retained by the custody store so a nested Saga's resources and compensation
/// handlers outlive the child invocation. Operator recovery
/// (`StreamReportHandle::reconcile_next_local_compensation_until`) reads them
/// through [`NestedCompensation`] to undo the child's retained work.
#[cfg(feature = "streaming")]
pub(crate) struct NestedSagaCustody<E, Res> {
    pub(crate) resources: Arc<Res>,
    pub(crate) handlers: ranvier_core::saga::SagaCompensationRegistry<E, Res>,
}
/// Boxed future returned by a type-erased nested compensation.
#[cfg(feature = "streaming")]
pub(crate) type NestedCompensationFuture<'a> = std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = Result<
                    crate::saga_report::SagaRollbackOutcome,
                    crate::saga_report::LocalTaskClaimError,
                >,
            > + Send
            + 'a,
    >,
>;
/// Type-erased reader of a retained nested Saga. The parent recovery owner
/// does not know the child's resource or error types. Tests downcast through
/// trait upcasting to `Arc<dyn Any>`.
#[cfg(feature = "streaming")]
pub(crate) trait NestedCompensation: Any + Send + Sync {
    /// Run one claimed child compensation with the child's own resources and
    /// frozen handler map. A missing handler settles the row as such.
    fn compensate<'a>(
        &'a self,
        claim: crate::saga_report::LocalTaskClaim,
        bus: &'a mut ranvier_core::Bus,
        deadline: tokio::time::Instant,
    ) -> NestedCompensationFuture<'a>;
}
#[cfg(feature = "streaming")]
impl<E, Res> NestedCompensation for NestedSagaCustody<E, Res>
where
    E: Send + Sync + 'static,
    Res: Send + Sync + 'static,
{
    fn compensate<'a>(
        &'a self,
        mut claim: crate::saga_report::LocalTaskClaim,
        bus: &'a mut ranvier_core::Bus,
        deadline: tokio::time::Instant,
    ) -> NestedCompensationFuture<'a> {
        Box::pin(async move {
            let Some(handler) = self.handlers.get(claim.node_id()) else {
                claim.mark_missing_handler()?;
                return Ok(crate::saga_report::SagaRollbackOutcome::MissingHandler);
            };
            claim
                .invoke(handler, self.resources.as_ref(), bus, deadline)
                .await
        })
    }
}
/// The custody-store record of a nested owned child.
#[cfg(feature = "streaming")]
pub(crate) struct NestedCustodyRecord(pub(crate) Arc<dyn NestedCompensation>);
pub(crate) type Capability = Arc<dyn Any + Send + Sync>;
struct CustodyRecord {
    owner: Capability,
    #[cfg(feature = "streaming")]
    task: Option<Capability>,
    #[cfg(feature = "streaming")]
    settlement: Option<crate::managed_stream_task::TaskSettlement>,
    #[cfg(feature = "streaming")]
    is_reservation: bool,
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
        let receipt = self
            .receipt
            .as_ref()
            .ok_or("execution.nested_reservation_decided")?;
        if !Weak::ptr_eq(&client.0, &receipt.store) {
            return Err("execution.nested_custody_mismatch");
        }
        let store = receipt
            .store
            .upgrade()
            .ok_or("execution.parent_custody_released")?;
        let records = store
            .records
            .lock()
            .map_err(|_| "execution.nested_custody_unavailable")?;
        if store.closed.load(Ordering::Acquire) {
            return Err("execution.parent_custody_released");
        }
        if store.admission_closed.load(Ordering::Acquire) {
            return Err("execution.nested_custody_admission_closed");
        }
        let record = records
            .get(&receipt.id)
            .ok_or("execution.parent_custody_released")?;
        if !Arc::ptr_eq(&record.owner, &self.marker) || record.task.is_some() {
            return Err("execution.nested_reservation_mismatch");
        }
        Ok(())
    }
    pub(crate) fn finalize_managed<T: Send + 'static>(
        &mut self,
        client: &WeakCustodyClient,
        owner: Capability,
        prepared: &crate::managed_stream_task::PreparedStreamTask<T>,
        journal: &crate::SagaRollbackJournal,
        observer: tokio::sync::watch::Receiver<crate::streaming_recovery::SupervisorObservation>,
    ) -> Result<WeakCustody, &'static str> {
        if !prepared.matches_owner(&owner) {
            return Err("execution.nested_task_owner_mismatch");
        }
        if !prepared.has_open_start() {
            return Err("execution.nested_task_start_decided");
        }
        self.finalize_with_settlement(
            client,
            owner,
            prepared.holder(),
            journal,
            observer,
            Some(prepared.settlement()),
        )
    }
    fn finalize_with_settlement(
        &mut self,
        client: &WeakCustodyClient,
        owner: Capability,
        task: Capability,
        journal: &crate::SagaRollbackJournal,
        observer: tokio::sync::watch::Receiver<crate::streaming_recovery::SupervisorObservation>,
        settlement: Option<crate::managed_stream_task::TaskSettlement>,
    ) -> Result<WeakCustody, &'static str> {
        if settlement
            .as_ref()
            .is_some_and(|settlement| !settlement.matches(&task))
        {
            return Err("execution.nested_task_registration_mismatch");
        }
        let receipt = self
            .receipt
            .as_ref()
            .ok_or("execution.nested_reservation_decided")?;
        if !Weak::ptr_eq(&client.0, &receipt.store) {
            return Err("execution.nested_custody_mismatch");
        }
        let store = receipt
            .store
            .upgrade()
            .ok_or("execution.parent_custody_released")?;
        let released = {
            let mut records = store
                .records
                .lock()
                .map_err(|_| "execution.nested_custody_unavailable")?;
            if store.closed.load(Ordering::Acquire) {
                return Err("execution.parent_custody_released");
            }
            // A closed drain snapshot must not miss a subsequently attached task.
            if store.admission_closed.load(Ordering::Acquire) {
                return Err("execution.nested_custody_admission_closed");
            }
            let record = records
                .get_mut(&receipt.id)
                .ok_or("execution.parent_custody_released")?;
            if !Arc::ptr_eq(&record.owner, &self.marker) || record.task.is_some() {
                return Err("execution.nested_reservation_mismatch");
            }
            journal.retain_stream_child(observer);
            std::mem::replace(
                record,
                CustodyRecord {
                    owner: owner.clone(),
                    task: Some(task),
                    settlement,
                    is_reservation: false,
                },
            )
        };
        let registered = WeakCustody {
            id: receipt.id,
            record: Arc::downgrade(&owner),
            store: receipt.store.clone(),
        };
        self.receipt = None;
        drop(released);
        Ok(registered)
    }
}
#[cfg(feature = "streaming")]
impl Drop for CustodyReservation {
    fn drop(&mut self) {
        let Some(receipt) = &self.receipt else {
            return;
        };
        let Some(store) = receipt.store.upgrade() else {
            return;
        };
        let released = {
            let mut records = store
                .records
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if records.get(&receipt.id).is_some_and(|record| {
                Arc::ptr_eq(&record.owner, &self.marker) && record.task.is_none()
            }) {
                records.remove(&receipt.id)
            } else {
                None
            }
        };
        drop(released);
    }
}

impl CustodyStore {
    #[cfg(feature = "streaming")]
    pub(crate) fn reserve_stream_slot(&self) -> Result<CustodyReservation, &'static str> {
        let marker: Capability = Arc::new(());
        let receipt = reserve_in_with_kind(&self.0.0, marker.clone(), None, || {}, true)?;
        Ok(CustodyReservation {
            receipt: Some(receipt),
            marker,
        })
    }
    #[cfg(any(test, feature = "streaming"))]
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
    #[cfg(any(test, feature = "streaming"))]
    pub(crate) fn close_admission(&self) -> Result<(), &'static str> {
        let _records = self
            .0
            .0
            .records
            .lock()
            .map_err(|_| "execution.nested_custody_unavailable")?;
        self.0.0.admission_closed.store(true, Ordering::Release);
        Ok(())
    }
    #[cfg(feature = "streaming")]
    pub(crate) fn close_and_erased_family(&self) -> Result<ErasedTaskFamily, &'static str> {
        let records = self
            .0
            .0
            .records
            .lock()
            .map_err(|_| "execution.nested_custody_unavailable")?;
        self.0.0.admission_closed.store(true, Ordering::Release);
        let mut tasks = Vec::new();
        let mut reserved = 0;
        for (id, record) in records.iter() {
            if record.task.is_none() {
                if !record.is_reservation {
                    return Err("execution.nested_unmanaged_owner");
                }
                reserved += 1;
                continue;
            }
            let settlement = record
                .settlement
                .clone()
                .ok_or("execution.nested_task_settlement_missing")?;
            tasks.push((*id, record.owner.clone(), settlement));
        }
        Ok(ErasedTaskFamily {
            tasks,
            statuses: Vec::new(),
            reserved,
        })
    }
    #[cfg(any(test, feature = "streaming"))]
    pub(crate) fn client(&self) -> WeakCustodyClient {
        WeakCustodyClient(Arc::downgrade(&self.0.0))
    }
    pub(crate) fn reserve(&self, record: Capability) -> Result<WeakCustody, &'static str> {
        reserve_in(&self.0.0, record)
    }
}
impl WeakCustodyClient {
    #[cfg(any(test, feature = "streaming"))]
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
    #[cfg(feature = "streaming")]
    pub(crate) fn validate_staged_task(
        &self,
        receipt: &WeakCustody,
        owner: &Capability,
        task: &Capability,
    ) -> Result<(), &'static str> {
        if !Weak::ptr_eq(&self.0, &receipt.store) {
            return Err("execution.nested_custody_mismatch");
        }
        let store = self
            .0
            .upgrade()
            .ok_or("execution.parent_custody_released")?;
        let records = store
            .records
            .lock()
            .map_err(|_| "execution.nested_custody_unavailable")?;
        if store.closed.load(Ordering::Acquire) {
            return Err("execution.parent_custody_released");
        }
        if store.admission_closed.load(Ordering::Acquire) {
            return Err("execution.nested_custody_admission_closed");
        }
        let record = records
            .get(&receipt.id)
            .ok_or("execution.parent_custody_released")?;
        if !Arc::ptr_eq(&record.owner, owner)
            || !Weak::ptr_eq(&receipt.record, &Arc::downgrade(owner))
        {
            return Err("execution.nested_task_owner_mismatch");
        }
        if !record
            .task
            .as_ref()
            .is_some_and(|registered| Arc::ptr_eq(registered, task))
            || record.settlement.is_none()
        {
            return Err("execution.nested_task_registration_mismatch");
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
    store: &Arc<StoreInner>,
    record: Capability,
    task: Option<Capability>,
    register: impl FnOnce(),
) -> Result<WeakCustody, &'static str> {
    reserve_in_with_kind(store, record, task, register, false)
}
fn reserve_in_with_kind(
    store: &Arc<StoreInner>,
    record: Capability,
    task: Option<Capability>,
    register: impl FnOnce(),
    is_reservation: bool,
) -> Result<WeakCustody, &'static str> {
    let _ = is_reservation;
    #[cfg(not(feature = "streaming"))]
    let _ = task;
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
    records.insert(
        id,
        CustodyRecord {
            owner: record,
            #[cfg(feature = "streaming")]
            task,
            #[cfg(feature = "streaming")]
            settlement: None,
            #[cfg(feature = "streaming")]
            is_reservation,
        },
    );
    register();
    Ok(weak)
}
#[cfg(feature = "streaming")]
pub(crate) struct ErasedTaskFamily {
    tasks: Vec<(
        uuid::Uuid,
        Capability,
        crate::managed_stream_task::TaskSettlement,
    )>,
    pub(crate) statuses: Vec<(
        uuid::Uuid,
        Result<(), crate::managed_stream_task::ManagedTaskWaitError>,
    )>,
    pub(crate) reserved: usize,
}
#[cfg(feature = "streaming")]
impl ErasedTaskFamily {
    pub(crate) fn task_ids(&self) -> Vec<uuid::Uuid> {
        self.tasks.iter().map(|(id, _, _)| *id).collect()
    }
    pub(crate) async fn settle_until(&mut self, deadline: tokio::time::Instant) {
        use futures_util::{StreamExt, stream::FuturesUnordered};
        let mut pending = FuturesUnordered::new();
        for (id, _, task) in &self.tasks {
            pending.push(async move { (*id, task.settle_until(deadline).await) });
        }
        self.statuses.clear();
        while let Some(status) = pending.next().await {
            self.statuses.push(status);
        }
    }
}
impl WeakCustody {
    #[cfg(feature = "streaming")]
    pub(crate) fn grant_task_start(
        &self,
        task: &Capability,
        permit: &mut Option<tokio::sync::oneshot::Sender<()>>,
    ) -> Result<(), &'static str> {
        let store = self
            .store
            .upgrade()
            .ok_or("execution.parent_custody_released")?;
        let records = store
            .records
            .lock()
            .map_err(|_| "execution.nested_custody_unavailable")?;
        let record = records
            .get(&self.id)
            .ok_or("execution.parent_custody_released")?;
        if store.closed.load(Ordering::Acquire) {
            return Err("execution.parent_custody_released");
        }
        if !Weak::ptr_eq(&self.record, &Arc::downgrade(&record.owner)) {
            return Err("execution.nested_task_owner_mismatch");
        }
        if !record
            .task
            .as_ref()
            .is_some_and(|registered| Arc::ptr_eq(registered, task))
        {
            return Err("execution.nested_task_registration_mismatch");
        }
        permit
            .take()
            .ok_or("execution.nested_task_start_decided")?
            .send(())
            .map_err(|_| "execution.nested_task_start_unavailable")
    }
    #[cfg(feature = "streaming")]
    pub(crate) fn upgrade(&self) -> Option<Capability> {
        self.record.upgrade()
    }
    pub(crate) fn release(&self) {
        if let Some(store) = self.store.upgrade() {
            let released = {
                let mut records = store.records.lock().unwrap_or_else(|p| p.into_inner());
                if records.get(&self.id).is_some_and(|record| {
                    Weak::ptr_eq(&self.record, &Arc::downgrade(&record.owner))
                }) {
                    records.remove(&self.id)
                } else {
                    None
                }
            };
            // User resources/handlers may have destructors; never release under lock.
            drop(released);
        }
    }
    /// Identity-checked removal primitive, NOT execution/reconciliation proof.
    /// The framework caller must establish the original terminal and keep any
    /// required recovery guards held through this call. Busy/poisoned registries
    /// and missing records are failures; they never imply prior reconciliation.
    #[cfg(feature = "streaming")]
    pub(crate) fn release_exact_task(
        &self,
        owner: &Capability,
        task: &Capability,
    ) -> Result<(), &'static str> {
        let store = self
            .store
            .upgrade()
            .ok_or("execution.parent_custody_released")?;
        let released = {
            let mut records = store
                .records
                .try_lock()
                .map_err(|_| "execution.nested_custody_unavailable")?;
            if store.closed.load(Ordering::Acquire) {
                return Err("execution.parent_custody_released");
            }
            let record = records
                .get(&self.id)
                .ok_or("execution.parent_custody_released")?;
            if !Weak::ptr_eq(&self.record, &Arc::downgrade(&record.owner))
                || !Arc::ptr_eq(owner, &record.owner)
                || !record
                    .task
                    .as_ref()
                    .is_some_and(|registered| Arc::ptr_eq(task, registered))
                || !record
                    .settlement
                    .as_ref()
                    .is_some_and(|settlement| settlement.matches(task))
                || record.is_reservation
            {
                return Err("execution.nested_task_registration_mismatch");
            }
            records.remove(&self.id)
        };
        // Destructors may re-enter custody; all user resources drop outside lock.
        drop(released);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "streaming")]
    fn slot_observer()
    -> tokio::sync::watch::Receiver<crate::streaming_recovery::SupervisorObservation> {
        tokio::sync::watch::channel(crate::streaming_recovery::SupervisorObservation {
            phase: crate::streaming_recovery::SupervisorPhase::NotStarted,
            disposition: None,
            cleanup_complete: None,
            cleanup_timed_out: false,
            custody: crate::streaming_recovery::RecoveryCustody::Held,
        })
        .1
    }
    #[cfg(feature = "streaming")]
    #[tokio::test]
    async fn atomic_managed_stage_racing_close_has_no_partial_task_snapshot() {
        use crate::managed_stream_task::PreparedStreamTask;
        for _ in 0..12 {
            let root = CustodyStore::new(1);
            let client = root.client();
            let mut slot = root.reserve_stream_slot().unwrap();
            let effects = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let owner: Capability = effects.clone();
            let observed = effects.clone();
            let prepared = PreparedStreamTask::new_owned(owner.clone(), async move {
                observed.fetch_add(1, Ordering::SeqCst);
                331u32
            });
            let holder = prepared.holder();
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let ready = barrier.clone();
            let worker = std::thread::spawn(move || {
                let journal = crate::SagaRollbackJournal::new("atomic-stage-close");
                ready.wait();
                let result =
                    slot.finalize_managed(&client, owner, &prepared, &journal, slot_observer());
                drop(prepared); // Reject unopened gate; root retains a published holder.
                result
            });
            barrier.wait();
            let mut family = root.close_and_erased_family().unwrap();
            let stage = worker.join().unwrap();
            match stage {
                Ok(_) => assert_eq!(family.tasks.len(), 1),
                Err("execution.nested_custody_admission_closed") => {
                    assert!(family.tasks.is_empty())
                }
                Err(other) => panic!("unexpected stage error: {other}"),
            }
            family
                .settle_until(tokio::time::Instant::now() + std::time::Duration::from_secs(5))
                .await;
            assert!(family.statuses.iter().all(|(_, result)| result.is_ok()));
            if !family.tasks.is_empty() {
                assert!(matches!(holder.lock().await.take_joined(), Some(Ok(None))));
            }
            assert_eq!(effects.load(Ordering::SeqCst), 0);
        }
    }
    #[cfg(feature = "streaming")]
    #[test]
    fn heterogeneous_snapshot_never_treats_live_unmanaged_owner_as_reservation() {
        let root = CustodyStore::new(1);
        let receipt = root.reserve(Arc::new(311u32)).unwrap();
        assert!(matches!(
            root.close_and_erased_family(),
            Err("execution.nested_unmanaged_owner")
        ));
        assert!(receipt.upgrade().is_some());
        assert!(root.reserve_stream_slot().is_err());
        receipt.release();
        let family = root.close_and_erased_family().unwrap();
        assert_eq!(family.reserved, 0);
        assert!(family.tasks.is_empty());
    }

    #[test]
    fn admission_closure_cannot_pass_framework_registration_gate() {
        let root = CustodyStore::new(1);
        let core = root.0.0.clone();
        let registered = Arc::new(AtomicBool::new(false));
        let recorded = registered.clone();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let reserve = std::thread::spawn(move || {
            reserve_in_with(&core, Arc::new(47u32), None, || {
                entered_tx.send(()).unwrap();
                release_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                recorded.store(true, Ordering::Release);
            })
        });
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert!(root.0.0.records.try_lock().is_err());
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
        assert!(matches!(
            root.reserve(Arc::new(())),
            Err("execution.nested_custody_admission_closed")
        ));
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
                client
                    .reserve(record.clone())
                    .map(|admission| (admission, record))
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
        assert_eq!(root.0.0.records.lock().unwrap().len(), accepted.len() + 1);
        assert!(
            root.client()
                .validate_member(&anchor_admission, &anchor)
                .is_ok()
        );
        for (admission, record) in &accepted {
            assert!(root.client().validate_member(admission, record).is_ok());
        }
        assert!(matches!(
            root.reserve(Arc::new(())),
            Err("execution.nested_custody_admission_closed")
        ));
        for (admission, _) in accepted {
            admission.release();
        }
        anchor_admission.release();
        assert!(root.0.0.records.lock().unwrap().is_empty());
    }
    #[test]
    fn admission_close_fails_closed_on_poison_without_retiring_records() {
        let root = CustodyStore::new(1);
        let admission = root.reserve(Arc::new(43u32)).unwrap();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _held = root.0.0.records.lock().unwrap();
            panic!("fixture poison");
        }));
        assert_eq!(
            root.close_admission(),
            Err("execution.nested_custody_unavailable")
        );
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
        assert!(matches!(
            client.reserve(Arc::new(())),
            Err("execution.nested_custody_admission_closed")
        ));
        assert!(client.validate_member(&admission, &record).is_ok());
        drop(record);
        assert!(weak.upgrade().is_some());
        drop(root);
        assert!(weak.upgrade().is_none());
        assert!(matches!(
            client.reserve(Arc::new(())),
            Err("execution.parent_custody_released")
        ));
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
                assert_eq!(selected.0.0, 42);
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
        let core = root.0.0.clone();
        assert!(
            std::thread::spawn(move || {
                let _held = core.records.lock().unwrap();
                panic!("fixture poison");
            })
            .join()
            .is_err()
        );
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
                store: Arc::downgrade(&store.0.0),
                released: released.clone(),
            }))
            .unwrap_or_else(|_| panic!("admission failed"));
        let refused = Arc::new(std::sync::atomic::AtomicBool::new(false));
        assert!(
            store
                .reserve(Arc::new(Probe {
                    store: Arc::downgrade(&store.0.0),
                    released: refused.clone()
                }))
                .is_err()
        );
        assert!(refused.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!released.load(std::sync::atomic::Ordering::SeqCst));
        retained.release();
        assert!(released.load(std::sync::atomic::Ordering::SeqCst));
        assert!(retained.upgrade().is_none());
        assert!(store.reserve(Arc::new(())).is_ok());
    }
}
