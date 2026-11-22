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
    #[cfg(test)]
    pub(crate) fn finalize_stream(
        &mut self,
        client: &WeakCustodyClient,
        owner: Capability,
        task: Capability,
        journal: &crate::SagaRollbackJournal,
        observer: tokio::sync::watch::Receiver<crate::streaming_recovery::SupervisorObservation>,
    ) -> Result<WeakCustody, &'static str> {
        self.finalize_with_settlement(client, owner, task, journal, observer, None)
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
    /// Root-only snapshot after admission closure. Cloning holders preserves
    /// unjoined task custody; this neither removes records nor proves join.
    #[cfg(all(test, feature = "streaming"))]
    pub(crate) fn closed_tasks(&self) -> Result<Vec<(uuid::Uuid, Capability)>, &'static str> {
        let records = self
            .0
            .0
            .records
            .lock()
            .map_err(|_| "execution.nested_custody_unavailable")?;
        if !self.0.0.admission_closed.load(Ordering::Acquire) {
            return Err("execution.nested_custody_admission_open");
        }
        Ok(records
            .iter()
            .filter_map(|(id, record)| record.task.as_ref().map(|task| (*id, task.clone())))
            .collect())
    }
    /// Acquire result custody before waiting, so dropped waits and root/record
    /// retirement cannot discard reports while this family lease remains owned.
    #[cfg(feature = "streaming")]
    #[cfg(test)]
    pub(crate) fn closed_typed_family<T: Send + 'static>(
        &self,
    ) -> Result<ManagedTaskFamily<T>, &'static str> {
        let tasks = self.closed_tasks()?.into_iter().map(|(id, holder)| {
            holder.downcast::<tokio::sync::Mutex<crate::managed_stream_task::ManagedStreamTask<T>>>()
                .map(|task| (id, task)).map_err(|_| "execution.nested_task_type_mismatch")
        }).collect::<Result<Vec<_>, _>>()?;
        Ok(ManagedTaskFamily {
            tasks,
            statuses: Vec::new(),
        })
    }
    #[cfg(feature = "streaming")]
    #[cfg(test)]
    pub(crate) async fn settle_closed_tasks<T: Send + 'static>(
        &self,
        deadline: tokio::time::Instant,
    ) -> Result<ManagedTaskFamily<T>, &'static str> {
        let mut family = self.closed_typed_family()?;
        family.settle_until(deadline).await;
        Ok(family)
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
/// Private typed family lease; no serialization or Debug of task outputs.
#[cfg(feature = "streaming")]
#[cfg(test)]
pub(crate) struct ManagedTaskFamily<T> {
    tasks: Vec<(
        uuid::Uuid,
        Arc<tokio::sync::Mutex<crate::managed_stream_task::ManagedStreamTask<T>>>,
    )>,
    statuses: Vec<(
        uuid::Uuid,
        Result<(), crate::managed_stream_task::ManagedTaskWaitError>,
    )>,
}
#[cfg(feature = "streaming")]
#[cfg(test)]
impl<T> std::ops::Deref for ManagedTaskFamily<T> {
    type Target = [(
        uuid::Uuid,
        Result<(), crate::managed_stream_task::ManagedTaskWaitError>,
    )];
    fn deref(&self) -> &Self::Target {
        &self.statuses
    }
}
#[cfg(feature = "streaming")]
#[cfg(test)]
impl<T: Send + 'static> ManagedTaskFamily<T> {
    pub(crate) async fn settle_until(&mut self, deadline: tokio::time::Instant) {
        use futures_util::{StreamExt, stream::FuturesUnordered};
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
        while let Some(result) = pending.next().await {
            self.statuses.push(result);
        }
    }
    pub(crate) async fn take_joined(
        &self,
        id: uuid::Uuid,
        deadline: tokio::time::Instant,
    ) -> Result<Option<Result<Option<T>, tokio::task::JoinError>>, &'static str> {
        let task = self
            .tasks
            .iter()
            .find(|(candidate, _)| *candidate == id)
            .map(|(_, task)| task)
            .ok_or("execution.nested_task_not_found")?;
        let mut task = tokio::time::timeout_at(deadline, task.lock())
            .await
            .map_err(|_| "execution.nested_task_transfer_timeout")?;
        Ok(task.take_joined())
    }
}
impl WeakCustodyClient {
    #[cfg(feature = "streaming")]
    #[cfg(test)]
    pub(crate) fn reserve_stream(
        &self,
        record: Capability,
        journal: &crate::SagaRollbackJournal,
        task: Option<Capability>,
        observer: tokio::sync::watch::Receiver<crate::streaming_recovery::SupervisorObservation>,
    ) -> Result<WeakCustody, &'static str> {
        let store = self
            .0
            .upgrade()
            .ok_or("execution.parent_custody_released")?;
        reserve_in_with(&store, record, task, || {
            journal.retain_stream_child(observer)
        })
    }
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
    /// Validate the exact registered task and send its framework start gate
    /// under the retirement lock. No user future is polled under this lock.
    #[cfg(feature = "streaming")]
    #[cfg(test)]
    pub(crate) fn attach_settlement(
        &self,
        settlement: crate::managed_stream_task::TaskSettlement,
    ) -> Result<(), &'static str> {
        let store = self
            .store
            .upgrade()
            .ok_or("execution.parent_custody_released")?;
        let mut records = store
            .records
            .lock()
            .map_err(|_| "execution.nested_custody_unavailable")?;
        if store.closed.load(Ordering::Acquire) {
            return Err("execution.parent_custody_released");
        }
        let record = records
            .get_mut(&self.id)
            .ok_or("execution.parent_custody_released")?;
        if !Weak::ptr_eq(&self.record, &Arc::downgrade(&record.owner)) {
            return Err("execution.nested_task_owner_mismatch");
        }
        if !record
            .task
            .as_ref()
            .is_some_and(|holder| settlement.matches(holder))
        {
            return Err("execution.nested_task_registration_mismatch");
        }
        if record.settlement.is_some() {
            return Err("execution.nested_task_settlement_decided");
        }
        record.settlement = Some(settlement);
        Ok(())
    }
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
    /// In-process custody correlation only; never a persisted event identity.
    #[cfg(feature = "streaming")]
    #[cfg(test)]
    pub(crate) fn id(&self) -> uuid::Uuid {
        self.id
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
    #[tokio::test]
    async fn exact_task_release_requires_original_identity_and_settlement() {
        let root = CustodyStore::new(1);
        let owner: Capability = Arc::new(347u32);
        let mut prepared =
            crate::managed_stream_task::PreparedStreamTask::new_owned(owner.clone(), async {
                349u32
            });
        let task: Capability = prepared.holder();
        let receipt = reserve_in_with(&root.0.0, owner.clone(), Some(task.clone()), || {}).unwrap();
        assert_eq!(
            receipt.release_exact_task(&owner, &task),
            Err("execution.nested_task_registration_mismatch")
        );
        receipt.attach_settlement(prepared.settlement()).unwrap();
        let foreign_owner: Capability = Arc::new(347u32);
        let foreign_task: Capability = Arc::new(349u32);
        let mut stale_receipt = receipt.clone();
        stale_receipt.record = Arc::downgrade(&foreign_owner);
        assert_eq!(
            stale_receipt.release_exact_task(&owner, &task),
            Err("execution.nested_task_registration_mismatch")
        );
        assert_eq!(
            receipt.release_exact_task(&foreign_owner, &task),
            Err("execution.nested_task_registration_mismatch")
        );
        assert_eq!(
            receipt.release_exact_task(&owner, &foreign_task),
            Err("execution.nested_task_registration_mismatch")
        );
        {
            let _held = root.0.0.records.lock().unwrap();
            assert_eq!(
                receipt.release_exact_task(&owner, &task),
                Err("execution.nested_custody_unavailable")
            );
        }
        assert_eq!(root.0.0.records.lock().unwrap().len(), 1);
        // Fixture caller joins the actual rejected gate before identity removal.
        assert!(matches!(
            prepared
                .reject_until(tokio::time::Instant::now() + std::time::Duration::from_secs(5))
                .await,
            Ok(Ok(None))
        ));
        root.close_admission().unwrap();
        assert_eq!(receipt.release_exact_task(&owner, &task), Ok(()));
        assert_eq!(
            receipt.release_exact_task(&owner, &task),
            Err("execution.parent_custody_released")
        );
        assert!(root.0.0.records.lock().unwrap().is_empty());
    }
    #[cfg(feature = "streaming")]
    #[tokio::test]
    async fn exact_task_release_preserves_poisoned_record() {
        let root = CustodyStore::new(1);
        let owner: Capability = Arc::new(353u32);
        let mut prepared =
            crate::managed_stream_task::PreparedStreamTask::new_owned(owner.clone(), async {});
        let task: Capability = prepared.holder();
        let receipt = reserve_in_with(&root.0.0, owner.clone(), Some(task.clone()), || {}).unwrap();
        receipt.attach_settlement(prepared.settlement()).unwrap();
        let core = root.0.0.clone();
        assert!(
            std::thread::spawn(move || {
                let _held = core.records.lock().unwrap();
                panic!("fixture poison");
            })
            .join()
            .is_err()
        );
        assert_eq!(
            receipt.release_exact_task(&owner, &task),
            Err("execution.nested_custody_unavailable")
        );
        match root.0.0.records.lock() {
            Err(poison) => assert_eq!(poison.into_inner().len(), 1),
            Ok(_) => panic!("fixture registry was not poisoned"),
        }
        assert!(matches!(
            prepared
                .reject_until(tokio::time::Instant::now() + std::time::Duration::from_secs(5))
                .await,
            Ok(Ok(None))
        ));
    }
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
    #[cfg(feature = "streaming")]
    #[tokio::test(start_paused = true)]
    async fn heterogeneous_family_settles_concurrently_and_retains_original_reports_after_root_drop()
     {
        use crate::managed_stream_task::PreparedStreamTask;
        let root = CustodyStore::new(2);
        let owner: Capability = Arc::new(271u32);
        let weak = Arc::downgrade(&owner);
        let mut first = PreparedStreamTask::new_owned(owner.clone(), async {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            277u32
        });
        let mut second = PreparedStreamTask::new_owned(owner.clone(), async {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            String::from("original second report")
        });
        let journal = crate::SagaRollbackJournal::new("heterogeneous-family");
        let a = root
            .client()
            .reserve_stream(
                owner.clone(),
                &journal,
                Some(first.holder()),
                slot_observer(),
            )
            .unwrap();
        let b = root
            .client()
            .reserve_stream(
                owner.clone(),
                &journal,
                Some(second.holder()),
                slot_observer(),
            )
            .unwrap();
        a.attach_settlement(first.settlement()).unwrap();
        b.attach_settlement(second.settlement()).unwrap();
        first.grant_registered_start(&a).unwrap();
        second.grant_registered_start(&b).unwrap();
        let first_holder = first.holder();
        let second_holder = second.holder();
        let mut family = root.close_and_erased_family().unwrap();
        drop(first);
        drop(second);
        drop(root);
        drop(owner);
        assert!(weak.upgrade().is_some());
        let started = tokio::time::Instant::now();
        family
            .settle_until(started + std::time::Duration::from_secs(3))
            .await;
        assert_eq!(
            tokio::time::Instant::now() - started,
            std::time::Duration::from_secs(2)
        );
        assert_eq!(family.statuses.len(), 2);
        assert!(family.statuses.iter().all(|(_, result)| result.is_ok()));
        assert!(matches!(
            first_holder.lock().await.take_joined(),
            Some(Ok(Some(277)))
        ));
        assert!(
            matches!(second_holder.lock().await.take_joined(), Some(Ok(Some(value))) if value == "original second report")
        );
        assert!(weak.upgrade().is_some());
        drop(family);
        assert!(weak.upgrade().is_none());
    }
    #[cfg(feature = "streaming")]
    #[tokio::test]
    async fn heterogeneous_snapshot_rejects_missing_or_foreign_settlement_before_polling() {
        use crate::managed_stream_task::PreparedStreamTask;
        let root = CustodyStore::new(1);
        let owner: Capability = Arc::new(281u32);
        let mut prepared = PreparedStreamTask::new_owned(owner.clone(), async { 283u32 });
        let mut other =
            PreparedStreamTask::new_owned(owner.clone(), async { String::from("foreign") });
        let journal = crate::SagaRollbackJournal::new("missing-settlement");
        let receipt = root
            .client()
            .reserve_stream(owner, &journal, Some(prepared.holder()), slot_observer())
            .unwrap();
        assert_eq!(
            receipt.attach_settlement(other.settlement()),
            Err("execution.nested_task_registration_mismatch")
        );
        assert!(matches!(
            root.close_and_erased_family(),
            Err("execution.nested_task_settlement_missing")
        ));
        receipt.attach_settlement(prepared.settlement()).unwrap();
        assert_eq!(
            receipt.attach_settlement(prepared.settlement()),
            Err("execution.nested_task_settlement_decided")
        );
        prepared.grant_registered_start(&receipt).unwrap();
        let mut family = root.close_and_erased_family().unwrap();
        family
            .settle_until(tokio::time::Instant::now() + std::time::Duration::from_secs(5))
            .await;
        assert!(family.statuses[0].1.is_ok());
        assert!(matches!(
            prepared.holder().lock().await.take_joined(),
            Some(Ok(Some(283)))
        ));
        other.reject_start().unwrap();
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
            observed.fetch_add(1, Ordering::SeqCst);
            191u32
        });
        let journal = crate::SagaRollbackJournal::new("reserved-root");
        let registered = reservation
            .finalize_stream(
                &root.client(),
                owner.clone(),
                prepared.holder(),
                &journal,
                slot_observer(),
            )
            .unwrap();
        assert_eq!(registered.id(), id);
        assert_eq!(root.0.0.records.lock().unwrap().len(), 1);
        assert_eq!(journal.retained_stream_observations().len(), 1);
        assert!(matches!(
            reservation.finalize_stream(
                &root.client(),
                owner,
                prepared.holder(),
                &journal,
                slot_observer()
            ),
            Err("execution.nested_reservation_decided")
        ));
        drop(reservation);
        assert!(registered.upgrade().is_some());
        assert_eq!(
            prepared.grant_registered_start(&stale),
            Err("execution.nested_task_owner_mismatch")
        );
        stale.release();
        assert!(registered.upgrade().is_some());
        assert_eq!(root.0.0.records.lock().unwrap().len(), 1);
        prepared.grant_registered_start(&registered).unwrap();
        assert!(matches!(
            prepared
                .holder()
                .lock()
                .await
                .join_until(tokio::time::Instant::now() + std::time::Duration::from_secs(5))
                .await,
            Ok(Ok(Some(191)))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[cfg(feature = "streaming")]
    #[tokio::test]
    async fn reserved_root_slot_rejection_has_no_phantom_observer_or_effect() {
        use crate::managed_stream_task::PreparedStreamTask;
        for mode in 0..3 {
            let root = CustodyStore::new(1);
            let foreign = CustodyStore::new(1);
            let client = if mode == 0 {
                foreign.client()
            } else {
                root.client()
            };
            let mut reservation = root.reserve_stream_slot().unwrap();
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let owner: Capability = calls.clone();
            let observed = calls.clone();
            let mut prepared = PreparedStreamTask::new_owned(owner.clone(), async move {
                observed.fetch_add(1, Ordering::SeqCst);
                193u32
            });
            let journal = crate::SagaRollbackJournal::new("rejected-root-slot");
            if mode == 1 {
                root.close_admission().unwrap();
            }
            let root = if mode == 2 {
                drop(root);
                None
            } else {
                Some(root)
            };
            let expected = match mode {
                0 => "execution.nested_custody_mismatch",
                1 => "execution.nested_custody_admission_closed",
                _ => "execution.parent_custody_released",
            };
            assert!(
                matches!(reservation.finalize_stream(&client, owner, prepared.holder(), &journal, slot_observer()), Err(error) if error == expected)
            );
            assert!(journal.retained_stream_observations().is_empty());
            drop(reservation);
            if let Some(root) = root {
                assert!(root.0.0.records.lock().unwrap().is_empty());
                if mode == 0 {
                    assert!(root.reserve_stream_slot().is_ok());
                } else {
                    assert!(root.reserve_stream_slot().is_err());
                }
            }
            assert!(matches!(
                prepared
                    .reject_until(tokio::time::Instant::now() + std::time::Duration::from_secs(5))
                    .await,
                Ok(Ok(None))
            ));
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
        let mut prepared = PreparedStreamTask::new(async {
            panic!("ordinary owner must not authorize start");
        });
        assert_eq!(
            prepared.grant_registered_start(&ordinary),
            Err("execution.nested_task_registration_mismatch")
        );
        assert!(matches!(
            prepared
                .reject_until(Instant::now() + std::time::Duration::from_secs(5))
                .await,
            Ok(Ok(None))
        ));
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
        let mut prepared = PreparedStreamTask::new(async move {
            observed.fetch_add(1, Ordering::SeqCst);
        });
        let admission =
            reserve_in_with(&root.0.0, Arc::new(()), Some(prepared.holder()), || {}).unwrap();
        let core = root.0.0.clone();
        assert!(
            std::thread::spawn(move || {
                let _held = core.records.lock().unwrap();
                panic!("fixture poison");
            })
            .join()
            .is_err()
        );
        assert_eq!(
            prepared.grant_registered_start(&admission),
            Err("execution.nested_custody_unavailable")
        );
        assert!(matches!(
            prepared
                .reject_until(Instant::now() + std::time::Duration::from_secs(5))
                .await,
            Ok(Ok(None))
        ));
        assert_eq!(effects.load(Ordering::SeqCst), 0);
        drop(root);
        assert_eq!(
            prepared.grant_registered_start(&admission),
            Err("execution.parent_custody_released")
        );
    }
    #[cfg(feature = "streaming")]
    #[tokio::test]
    async fn registered_start_rejects_other_holder_and_retired_record_without_consuming_gate() {
        use crate::managed_stream_task::PreparedStreamTask;
        use tokio::time::Instant;
        let root = CustodyStore::new(2);
        let effects = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = effects.clone();
        let mut first = PreparedStreamTask::new(async move {
            observed.fetch_add(1, Ordering::SeqCst);
        });
        let mut second = PreparedStreamTask::new(async {});
        let first_record =
            reserve_in_with(&root.0.0, Arc::new(()), Some(first.holder()), || {}).unwrap();
        let second_record =
            reserve_in_with(&root.0.0, Arc::new(()), Some(second.holder()), || {}).unwrap();
        root.close_admission().unwrap();
        assert_eq!(
            first.grant_registered_start(&second_record),
            Err("execution.nested_task_registration_mismatch")
        );
        assert_eq!(effects.load(Ordering::SeqCst), 0);
        first.grant_registered_start(&first_record).unwrap();
        assert!(matches!(
            first
                .holder()
                .lock()
                .await
                .join_until(Instant::now() + std::time::Duration::from_secs(5))
                .await,
            Ok(Ok(Some(())))
        ));
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        second_record.release();
        assert_eq!(
            second.grant_registered_start(&second_record),
            Err("execution.parent_custody_released")
        );
        assert!(matches!(
            second
                .reject_until(Instant::now() + std::time::Duration::from_secs(5))
                .await,
            Ok(Ok(None))
        ));
    }
    #[cfg(feature = "streaming")]
    #[tokio::test(start_paused = true)]
    async fn family_transfer_deadline_and_unknown_id_preserve_original_result() {
        use crate::managed_stream_task::ManagedStreamTask;
        use tokio::{sync::Mutex as AsyncMutex, time::Instant};
        let root = CustodyStore::new(1);
        let (task, permit) = ManagedStreamTask::prepare(async { 103u32 });
        let task = Arc::new(AsyncMutex::new(task));
        let admission =
            reserve_in_with(&root.0.0, Arc::new(()), Some(task.clone()), || {}).unwrap();
        root.close_admission().unwrap();
        permit.send(()).unwrap();
        let family = root
            .settle_closed_tasks::<u32>(Instant::now() + std::time::Duration::from_secs(5))
            .await
            .unwrap();
        assert!(matches!(
            family
                .take_joined(uuid::Uuid::new_v4(), Instant::now())
                .await,
            Err("execution.nested_task_not_found")
        ));
        let held = task.lock().await;
        assert!(matches!(
            family
                .take_joined(
                    admission.id,
                    Instant::now() + std::time::Duration::from_secs(3)
                )
                .await,
            Err("execution.nested_task_transfer_timeout")
        ));
        drop(held);
        assert_eq!(
            family
                .take_joined(admission.id, Instant::now())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            Some(103)
        );
    }
    #[cfg(feature = "streaming")]
    #[tokio::test]
    async fn family_lease_preserves_report_after_root_loss_and_dropped_wait() {
        use crate::managed_stream_task::ManagedStreamTask;
        use tokio::{
            sync::{Mutex as AsyncMutex, oneshot},
            time::Instant,
        };
        let root = CustodyStore::new(1);
        let (finish, gate) = oneshot::channel();
        let (task, permit) =
            ManagedStreamTask::prepare(async move { Box::new(gate.await.unwrap()) });
        let task = Arc::new(AsyncMutex::new(task));
        let admission = reserve_in_with(&root.0.0, Arc::new(()), Some(task), || {}).unwrap();
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
        family
            .settle_until(Instant::now() + std::time::Duration::from_secs(5))
            .await;
        assert_eq!(family.statuses, vec![(admission.id, Ok(()))]);
        assert_eq!(
            *family
                .take_joined(admission.id, Instant::now())
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .unwrap(),
            101
        );
        assert!(
            family
                .take_joined(admission.id, Instant::now())
                .await
                .unwrap()
                .is_none()
        );
    }
    #[cfg(feature = "streaming")]
    #[tokio::test]
    async fn concurrent_family_settlement_transfers_original_report_once() {
        use crate::managed_stream_task::ManagedStreamTask;
        use tokio::{sync::Mutex as AsyncMutex, time::Instant};
        let root = CustodyStore::new(1);
        let (task, permit) = ManagedStreamTask::prepare(async { Box::new(89u32) });
        let task = Arc::new(AsyncMutex::new(task));
        let admission =
            reserve_in_with(&root.0.0, Arc::new(()), Some(task.clone()), || {}).unwrap();
        root.close_admission().unwrap();
        permit.send(()).unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        let (first, second) = tokio::join!(
            root.settle_closed_tasks::<Box<u32>>(deadline),
            root.settle_closed_tasks::<Box<u32>>(deadline)
        );
        assert_eq!(first.unwrap().statuses, vec![(admission.id, Ok(()))]);
        assert_eq!(second.unwrap().statuses, vec![(admission.id, Ok(()))]);
        let (first, second) = tokio::join!(async { task.lock().await.take_joined() }, async {
            task.lock().await.take_joined()
        },);
        assert_eq!(
            usize::from(first.is_some()) + usize::from(second.is_some()),
            1
        );
        assert_eq!(*first.or(second).unwrap().unwrap().unwrap(), 89);
    }

    #[cfg(feature = "streaming")]
    #[tokio::test]
    async fn active_family_snapshot_survives_record_retirement() {
        use crate::managed_stream_task::ManagedStreamTask;
        use tokio::{
            sync::{Mutex as AsyncMutex, oneshot},
            time::Instant,
        };
        let root = CustodyStore::new(1);
        let (finish, gate) = oneshot::channel();
        let (task, permit) = ManagedStreamTask::prepare(async move { gate.await.unwrap() });
        let task = Arc::new(AsyncMutex::new(task));
        let weak_task = Arc::downgrade(&task);
        let admission =
            reserve_in_with(&root.0.0, Arc::new(()), Some(task.clone()), || {}).unwrap();
        root.close_admission().unwrap();
        permit.send(()).unwrap();
        drop(task);
        let wait =
            root.settle_closed_tasks::<u32>(Instant::now() + std::time::Duration::from_secs(5));
        tokio::pin!(wait);
        assert!(futures_util::poll!(&mut wait).is_pending());
        admission.release();
        assert!(root.closed_tasks().unwrap().is_empty());
        // Acquire explicit result custody before the snapshot can retire.
        let retained = weak_task.upgrade().unwrap();
        finish.send(97u32).unwrap();
        assert_eq!(wait.await.unwrap().statuses, vec![(admission.id, Ok(()))]);
        assert_eq!(
            retained.lock().await.take_joined().unwrap().unwrap(),
            Some(97)
        );
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
        let admission =
            reserve_in_with(&root.0.0, Arc::new(()), Some(task.clone()), || {}).unwrap();
        root.close_admission().unwrap();
        permit.send(()).unwrap();
        let held = task.lock().await;
        let result = root
            .settle_closed_tasks::<u32>(Instant::now() + std::time::Duration::from_secs(3))
            .await
            .unwrap();
        assert_eq!(
            result.statuses,
            vec![(admission.id, Err(ManagedTaskWaitError::TimedOut))]
        );
        drop(held);
        let result = root
            .settle_closed_tasks::<u32>(Instant::now() + std::time::Duration::from_secs(3))
            .await
            .unwrap();
        assert_eq!(result.statuses, vec![(admission.id, Ok(()))]);
        assert_eq!(task.lock().await.take_joined().unwrap().unwrap(), Some(83));
    }
    #[cfg(feature = "streaming")]
    #[tokio::test]
    async fn family_settlement_drop_preserves_completed_report_and_pending_handle() {
        use crate::managed_stream_task::ManagedStreamTask;
        use tokio::{
            sync::{Mutex as AsyncMutex, oneshot},
            time::Instant,
        };
        let root = CustodyStore::new(2);
        let report = Arc::new(67u32);
        let weak_report = Arc::downgrade(&report);
        let (ready, ready_gate) = oneshot::channel();
        let (first, first_permit) = ManagedStreamTask::prepare(async move {
            ready.send(()).unwrap();
            report
        });
        let (finish, finish_gate) = oneshot::channel();
        let (second, second_permit) =
            ManagedStreamTask::prepare(async move { Arc::new(finish_gate.await.unwrap()) });
        let first = Arc::new(AsyncMutex::new(first));
        let second = Arc::new(AsyncMutex::new(second));
        let first_record =
            reserve_in_with(&root.0.0, Arc::new(()), Some(first.clone()), || {}).unwrap();
        let second_record =
            reserve_in_with(&root.0.0, Arc::new(()), Some(second.clone()), || {}).unwrap();
        root.close_admission().unwrap();
        first_permit.send(()).unwrap();
        second_permit.send(()).unwrap();
        ready_gate.await.unwrap();
        {
            let wait = root.settle_closed_tasks::<Arc<u32>>(
                Instant::now() + std::time::Duration::from_secs(5),
            );
            tokio::pin!(wait);
            assert!(futures_util::poll!(&mut wait).is_pending());
        }
        assert!(weak_report.upgrade().is_some());
        // Completed output stays in the actual holder after the family future drops.
        let original = first.lock().await.take_joined().unwrap().unwrap().unwrap();
        assert_eq!(*original, 67);
        finish.send(71u32).unwrap();
        let settled = root
            .settle_closed_tasks::<Arc<u32>>(Instant::now() + std::time::Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(settled.len(), 2);
        assert!(settled.iter().any(|(id, status)| *id == first_record.id
            && *status == Err(crate::managed_stream_task::ManagedTaskWaitError::AlreadyJoined)));
        assert!(
            settled
                .iter()
                .any(|(id, status)| *id == second_record.id && *status == Ok(()))
        );
        assert_eq!(
            *second.lock().await.take_joined().unwrap().unwrap().unwrap(),
            71
        );
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
        reserve_in_with(&root.0.0, Arc::new(()), Some(first.clone()), || {}).unwrap();
        let mismatch =
            reserve_in_with(&root.0.0, Arc::new(()), Some(Arc::new(79u64)), || {}).unwrap();
        root.close_admission().unwrap();
        permit.send(()).unwrap();
        assert!(matches!(
            root.settle_closed_tasks::<u32>(Instant::now() + std::time::Duration::from_secs(5))
                .await,
            Err("execution.nested_task_type_mismatch")
        ));
        assert!(first.lock().await.take_joined().is_none());
        mismatch.release();
        assert!(
            root.settle_closed_tasks::<u32>(Instant::now() + std::time::Duration::from_secs(5))
                .await
                .unwrap()
                .iter()
                .all(|(_, result)| result.is_ok())
        );
        assert_eq!(first.lock().await.take_joined().unwrap().unwrap(), Some(73));
    }
    #[cfg(feature = "streaming")]
    #[test]
    fn closed_task_snapshot_excludes_plain_owners_and_keeps_holder_after_record_release() {
        let root = CustodyStore::new(2);
        let ordinary = root.reserve(Arc::new(53u32)).unwrap();
        let holder: Capability = Arc::new(59u32);
        let weak = Arc::downgrade(&holder);
        let managed = reserve_in_with(&root.0.0, Arc::new(61u32), Some(holder), || {}).unwrap();
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
