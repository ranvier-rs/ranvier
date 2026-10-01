use crate::domain::{Product, ValidatedOrder};
use anyhow::Result;
use async_trait::async_trait;
use ranvier_core::synapse::Synapse;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::time::{Duration, sleep};

// --- Inventory Synapse ---
pub struct InventorySynapse {
    // In-memory mock database
    pub inventory: Arc<Mutex<HashMap<String, Product>>>,
    reservations: Mutex<HashMap<String, Reservation>>,
}

#[derive(Clone, Copy, PartialEq)]
enum PaymentState {
    NotAttempted,
    Pending,
    Approved,
    Declined,
    Refunded,
}
struct Reservation {
    owner: String,
    counts: HashMap<String, u32>,
    amount: u32,
    released: bool,
    payment: PaymentState,
    fulfilled: bool,
}

impl InventorySynapse {
    pub fn authorize_shipping(&self, order: &ValidatedOrder) -> Result<(), String> {
        if order.request.items.is_empty() || order.request.items.len() > 1000 {
            return Err("Invalid shipment cart".into());
        }
        let mut requested_counts = HashMap::new();
        for id in &order.request.items {
            *requested_counts.entry(id.clone()).or_insert(0u32) += 1;
        }
        let ledger = self
            .reservations
            .lock()
            .map_err(|_| "Reservation ledger unavailable".to_owned())?;
        let record = ledger
            .get(&order.request.order_id)
            .ok_or_else(|| "Reservation missing".to_owned())?;
        if record.owner != order.reservation_owner
            || record.amount != order.payable_amount
            || record.amount != order.request.total_amount
            || record.counts != requested_counts
            || record.released
            || record.fulfilled
            || record.payment != PaymentState::Approved
        {
            return Err("Shipment requires reconciliation".into());
        }
        Ok(())
    }

    pub fn fulfill(&self, order: &ValidatedOrder) -> Result<(), String> {
        let mut ledger = self
            .reservations
            .lock()
            .map_err(|_| "Reservation ledger unavailable".to_owned())?;
        let record = ledger
            .get_mut(&order.request.order_id)
            .ok_or_else(|| "Reservation missing".to_owned())?;
        if record.owner != order.reservation_owner
            || record.released
            || record.payment != PaymentState::Approved
        {
            return Err("Fulfillment requires reconciliation".into());
        }
        record.fulfilled = true;
        Ok(())
    }
    pub fn new(inventory: Arc<Mutex<HashMap<String, Product>>>) -> Self {
        Self {
            inventory,
            reservations: Mutex::new(HashMap::new()),
        }
    }

    pub fn begin_payment(&self, order: &ValidatedOrder) -> Result<(), String> {
        let mut ledger = self
            .reservations
            .lock()
            .map_err(|_| "Reservation ledger unavailable".to_owned())?;
        let record = ledger
            .get_mut(&order.request.order_id)
            .ok_or_else(|| "Reservation missing".to_owned())?;
        if record.owner != order.reservation_owner
            || record.amount != order.payable_amount
            || record.amount != order.request.total_amount
            || record.released
            || record.payment != PaymentState::NotAttempted
        {
            return Err("Payment requires reconciliation".into());
        }
        record.payment = PaymentState::Pending;
        Ok(())
    }

    pub fn confirm_payment(&self, order: &ValidatedOrder, approved: bool) -> Result<(), String> {
        let mut ledger = self
            .reservations
            .lock()
            .map_err(|_| "Reservation ledger unavailable".to_owned())?;
        let record = ledger
            .get_mut(&order.request.order_id)
            .ok_or_else(|| "Reservation missing".to_owned())?;
        if record.owner != order.reservation_owner || record.payment != PaymentState::Pending {
            return Err("Payment requires reconciliation".into());
        }
        record.payment = if approved {
            PaymentState::Approved
        } else {
            PaymentState::Declined
        };
        Ok(())
    }

    pub fn release(&self, order: &ValidatedOrder) -> Result<(), String> {
        // All operations touching both stores acquire inventory before ledger.
        let mut db = self
            .inventory
            .lock()
            .map_err(|_| "Inventory unavailable".to_owned())?;
        let mut ledger = self
            .reservations
            .lock()
            .map_err(|_| "Reservation ledger unavailable".to_owned())?;
        let Some(record) = ledger.get_mut(&order.request.order_id) else {
            return Ok(());
        };
        if record.owner != order.reservation_owner {
            return Err("Reservation owner mismatch".into());
        }
        if record.released {
            return Ok(());
        }
        if matches!(
            record.payment,
            PaymentState::Pending | PaymentState::Approved
        ) {
            return Err("Payment requires reconciliation before release".into());
        }
        // Check every restoration first, including overflow/missing product.
        for (id, count) in &record.counts {
            db.get(id)
                .ok_or_else(|| "Reserved product missing".to_owned())?
                .stock
                .checked_add(*count)
                .ok_or_else(|| "Inventory restoration overflow".to_owned())?;
        }
        for (id, count) in &record.counts {
            let product = db
                .get_mut(id)
                .ok_or_else(|| "Reserved product missing".to_owned())?;
            product.stock += count;
        }
        record.released = true;
        Ok(())
    }

    pub fn settle_refund(
        &self,
        order: &ValidatedOrder,
        confirmation: RefundConfirmation,
    ) -> Result<(), String> {
        let mut ledger = self
            .reservations
            .lock()
            .map_err(|_| "Reservation ledger unavailable".to_owned())?;
        let Some(record) = ledger.get_mut(&order.request.order_id) else {
            return Ok(());
        };
        if record.owner != order.reservation_owner || record.amount != order.payable_amount {
            return Err("Refund owner or amount mismatch".into());
        }
        if confirmation.order_id != order.request.order_id
            || confirmation.owner != record.owner
            || confirmation.amount != record.amount
        {
            return Err("Refund confirmation mismatch".into());
        }
        if confirmation.refunded && confirmation.receipt.is_some() {
            record.payment = PaymentState::Refunded;
            Ok(())
        } else if matches!(
            record.payment,
            PaymentState::NotAttempted | PaymentState::Declined | PaymentState::Refunded
        ) {
            Ok(())
        } else {
            Err("Payment requires reconciliation".into())
        }
    }
    pub fn quote(&self, items: &[String]) -> Result<(Vec<u32>, u32), String> {
        let db = self
            .inventory
            .lock()
            .map_err(|_| "Inventory unavailable".to_owned())?;
        Self::quote_locked(&db, items)
    }

    fn quote_locked(
        db: &HashMap<String, Product>,
        items: &[String],
    ) -> Result<(Vec<u32>, u32), String> {
        if items.is_empty() || items.len() > 1000 {
            return Err("Invalid reservation size".into());
        }
        let mut prices = Vec::with_capacity(items.len());
        let mut total = 0u32;
        for id in items {
            let price = db
                .get(id)
                .ok_or_else(|| "Product not found".to_owned())?
                .price;
            if price == 0 {
                return Err("Invalid catalog price".into());
            }
            total = total
                .checked_add(price)
                .ok_or_else(|| "Catalog amount overflow".to_owned())?;
            prices.push(price);
        }
        Ok((prices, total))
    }
}

#[async_trait]
impl Synapse for InventorySynapse {
    type Input = ValidatedOrder;
    type Output = bool; // Success/Fail
    type Error = String;

    async fn call(&self, order: Self::Input) -> Result<Self::Output, Self::Error> {
        let item_ids = order.request.items;
        println!(
            "\x1b[36m[Inventory]\x1b[0m Checking stock for {} items...",
            item_ids.len()
        );
        sleep(Duration::from_millis(300)).await; // Latency

        if item_ids.is_empty() || item_ids.len() > 1000 {
            return Err("Invalid reservation size".into());
        }
        let mut counts: HashMap<String, u32> = HashMap::new();
        for id in &item_ids {
            *counts.entry(id.clone()).or_default() += 1;
        }
        let mut db = self
            .inventory
            .lock()
            .map_err(|_| "Inventory unavailable".to_owned())?;
        let (prices, total) = Self::quote_locked(&db, &item_ids)?;
        if prices != order.catalog_prices
            || total != order.payable_amount
            || total != order.request.total_amount
        {
            return Err("Catalog changed before reservation".into());
        }
        let mut ledger = self
            .reservations
            .lock()
            .map_err(|_| "Reservation ledger unavailable".to_owned())?;
        if ledger.contains_key(&order.request.order_id) || ledger.len() >= 1000 {
            return Err("Reservation requires reconciliation".into());
        }
        // Validate the whole cart before mutation under the same lock.
        for (id, count) in &counts {
            let product = db.get(id).ok_or_else(|| "Product not found".to_owned())?;
            if product.stock < *count {
                return Ok(false);
            }
        }
        for (id, count) in &counts {
            let product = db
                .get_mut(id)
                .ok_or_else(|| "Inventory changed".to_owned())?;
            product.stock -= *count;
        }
        ledger.insert(
            order.request.order_id,
            Reservation {
                owner: order.reservation_owner,
                counts,
                amount: order.payable_amount,
                released: false,
                payment: PaymentState::NotAttempted,
                fulfilled: false,
            },
        );
        Ok(true)
    }
}

// --- Payment Synapse ---
#[derive(Default)]
pub struct PaymentSynapse {
    pub attempted_amounts: Mutex<Vec<u32>>,
    records: Mutex<HashMap<String, PaymentRecord>>,
    #[cfg(test)]
    pub refund_behavior: RefundBehavior,
}
#[derive(Debug, Clone, Copy, PartialEq)]
enum ReceiptState {
    Pending,
    Declined,
    Approved,
    RefundPending,
    Refunded,
}
struct PaymentRecord {
    owner: String,
    amount: u32,
    receipt: Option<String>,
    outstanding: u32,
    state: ReceiptState,
    dispatch: DispatchState,
}

#[derive(Clone)]
enum DispatchState {
    NotStarted,
    Pending,
    Rejected,
    Shipped(String),
}

pub struct DispatchPermit {
    order_id: String,
    owner: String,
    amount: u32,
    receipt: String,
}

#[derive(Debug)]
pub enum DispatchError {
    Rejected,
    Uncertain,
}
#[cfg(test)]
#[derive(Default, Clone, Copy)]
pub enum RefundBehavior {
    #[default]
    Complete,
    Fault,
    Panic,
    Uncertain,
}

pub struct RefundConfirmation {
    order_id: String,
    owner: String,
    amount: u32,
    refunded: bool,
    receipt: Option<String>,
}
impl RefundConfirmation {
    fn from_order(order: &ValidatedOrder, refunded: bool, receipt: Option<String>) -> Self {
        Self {
            order_id: order.request.order_id.clone(),
            owner: order.reservation_owner.clone(),
            amount: order.payable_amount,
            refunded,
            receipt,
        }
    }
}

impl PaymentSynapse {
    pub fn confirm_tracking(&self, order: &ValidatedOrder, tracking: &str) -> Result<(), String> {
        let records = self
            .records
            .lock()
            .map_err(|_| "Payment ledger unavailable".to_owned())?;
        let record = records
            .get(&order.request.order_id)
            .ok_or_else(|| "Payment record missing".to_owned())?;
        if record.owner != order.reservation_owner
            || record.amount != order.payable_amount
            || record.state != ReceiptState::Approved
            || !matches!(&record.dispatch, DispatchState::Shipped(actual) if actual == tracking)
        {
            return Err("Fulfillment requires reconciliation".into());
        }
        Ok(())
    }
    pub fn begin_dispatch(&self, order: &ValidatedOrder) -> Result<DispatchPermit, String> {
        let mut records = self
            .records
            .lock()
            .map_err(|_| "Payment ledger unavailable".to_owned())?;
        let record = records
            .get_mut(&order.request.order_id)
            .ok_or_else(|| "Payment record missing".to_owned())?;
        if record.owner != order.reservation_owner
            || record.amount != order.payable_amount
            || record.amount != order.request.total_amount
            || record.state != ReceiptState::Approved
            || !matches!(record.dispatch, DispatchState::NotStarted)
        {
            return Err("Dispatch requires reconciliation".into());
        }
        let receipt = record
            .receipt
            .clone()
            .ok_or_else(|| "Payment receipt missing".to_owned())?;
        record.dispatch = DispatchState::Pending;
        Ok(DispatchPermit {
            order_id: order.request.order_id.clone(),
            owner: record.owner.clone(),
            amount: record.amount,
            receipt,
        })
    }

    pub fn finish_dispatch(
        &self,
        order: &ValidatedOrder,
        result: &Result<String, DispatchError>,
    ) -> Result<(), String> {
        let mut records = self
            .records
            .lock()
            .map_err(|_| "Payment ledger unavailable".to_owned())?;
        let record = records
            .get_mut(&order.request.order_id)
            .ok_or_else(|| "Payment record missing".to_owned())?;
        if record.owner != order.reservation_owner
            || !matches!(record.dispatch, DispatchState::Pending)
        {
            return Err("Dispatch requires reconciliation".into());
        }
        match result {
            Ok(tracking) => record.dispatch = DispatchState::Shipped(tracking.clone()),
            Err(DispatchError::Rejected) => record.dispatch = DispatchState::Rejected,
            Err(DispatchError::Uncertain) => return Err("Dispatch requires reconciliation".into()),
        }
        Ok(())
    }

    pub fn require_no_dispatch(&self, order: &ValidatedOrder) -> Result<(), String> {
        let records = self
            .records
            .lock()
            .map_err(|_| "Payment ledger unavailable".to_owned())?;
        let Some(record) = records.get(&order.request.order_id) else {
            return Ok(());
        };
        if record.owner != order.reservation_owner
            || !matches!(
                record.dispatch,
                DispatchState::NotStarted | DispatchState::Rejected
            )
        {
            return Err("Dispatch requires reconciliation before refund".into());
        }
        Ok(())
    }
    #[cfg(test)]
    pub fn test_balance(&self, id: &str) -> (u32, bool) {
        let ledger = self.records.lock().unwrap();
        let record = &ledger[id];
        (record.outstanding, record.state == ReceiptState::Refunded)
    }
    pub fn refund(&self, order: &ValidatedOrder) -> Result<RefundConfirmation, String> {
        {
            let mut records = self
                .records
                .lock()
                .map_err(|_| "Payment ledger unavailable".to_owned())?;
            let Some(record) = records.get_mut(&order.request.order_id) else {
                return Ok(RefundConfirmation::from_order(order, false, None));
            };
            if record.owner != order.reservation_owner || record.amount != order.payable_amount {
                return Err("Refund owner or amount mismatch".into());
            }
            if !matches!(
                record.dispatch,
                DispatchState::NotStarted | DispatchState::Rejected
            ) {
                return Err("Dispatch requires reconciliation before refund".into());
            }
            match record.state {
                ReceiptState::Declined => {
                    return Ok(RefundConfirmation::from_order(order, false, None));
                }
                ReceiptState::Refunded => {
                    return Ok(RefundConfirmation::from_order(
                        order,
                        true,
                        record.receipt.clone(),
                    ));
                }
                ReceiptState::Approved
                    if record.receipt.is_some() && record.outstanding == record.amount => {}
                _ => return Err("Refund requires reconciliation".into()),
            }
            record.state = ReceiptState::RefundPending;
        }
        // Explicit test failures occur outside the ledger lock to avoid poisoning.
        #[cfg(test)]
        match self.refund_behavior {
            RefundBehavior::Fault => return Err("Refund failed".into()),
            RefundBehavior::Uncertain => return Err("Refund outcome uncertain".into()),
            RefundBehavior::Panic => panic!("explicit test-only refund panic"),
            RefundBehavior::Complete => {}
        }
        let mut records = self
            .records
            .lock()
            .map_err(|_| "Payment ledger unavailable".to_owned())?;
        let record = records
            .get_mut(&order.request.order_id)
            .ok_or_else(|| "Payment record missing".to_owned())?;
        record.outstanding = 0;
        record.state = ReceiptState::Refunded;
        Ok(RefundConfirmation::from_order(
            order,
            true,
            record.receipt.clone(),
        ))
    }
}

#[async_trait]
impl Synapse for PaymentSynapse {
    type Input = ValidatedOrder;
    type Output = bool;
    type Error = String;
    async fn call(&self, order: Self::Input) -> Result<bool, String> {
        let amount = order.payable_amount;
        {
            let mut records = self
                .records
                .lock()
                .map_err(|_| "Payment ledger unavailable".to_owned())?;
            if amount == 0
                || amount != order.request.total_amount
                || records.contains_key(&order.request.order_id)
                || records.len() >= 1000
            {
                return Err("Payment requires reconciliation".into());
            }
            records.insert(
                order.request.order_id.clone(),
                PaymentRecord {
                    owner: order.reservation_owner,
                    amount,
                    receipt: None,
                    outstanding: 0,
                    state: ReceiptState::Pending,
                    dispatch: DispatchState::NotStarted,
                },
            );
        }
        self.attempted_amounts
            .lock()
            .map_err(|_| "Payment unavailable".to_owned())?
            .push(amount);
        sleep(Duration::from_millis(500)).await;
        let mut records = self
            .records
            .lock()
            .map_err(|_| "Payment ledger unavailable".to_owned())?;
        let record = records
            .get_mut(&order.request.order_id)
            .ok_or_else(|| "Payment record missing".to_owned())?;
        if amount > 1000 {
            record.state = ReceiptState::Declined;
            Ok(false)
        } else {
            record.receipt = Some(uuid::Uuid::new_v4().to_string());
            record.outstanding = amount;
            record.state = ReceiptState::Approved;
            Ok(true)
        }
    }
}

// --- Shipping Synapse ---
#[derive(Default)]
pub struct ShippingSynapse {
    records: Mutex<HashMap<String, ShipmentRecord>>,
    #[cfg(test)]
    pub behavior: ShippingBehavior,
}
struct ShipmentRecord {
    owner: String,
    amount: u32,
    receipt: String,
    tracking: Option<String>,
    rejected: bool,
}
#[cfg(test)]
#[derive(Default, Clone, Copy)]
pub enum ShippingBehavior {
    #[default]
    Complete,
    Reject,
    Uncertain,
}

impl ShippingSynapse {
    #[cfg(test)]
    pub fn test_tracking(&self, id: &str) -> Option<String> {
        self.records
            .lock()
            .unwrap()
            .get(id)
            .and_then(|record| record.tracking.clone())
    }
    pub fn confirm_tracking(&self, order: &ValidatedOrder, tracking: &str) -> Result<(), String> {
        let records = self
            .records
            .lock()
            .map_err(|_| "Shipment ledger unavailable".to_owned())?;
        let record = records
            .get(&order.request.order_id)
            .ok_or_else(|| "Shipment record missing".to_owned())?;
        if record.owner != order.reservation_owner
            || record.amount != order.payable_amount
            || record.receipt.is_empty()
            || record.rejected
            || record.tracking.as_deref() != Some(tracking)
        {
            return Err("Shipment requires reconciliation".into());
        }
        Ok(())
    }
}

#[async_trait]
impl Synapse for ShippingSynapse {
    type Input = DispatchPermit;
    type Output = String;
    type Error = DispatchError;
    async fn call(&self, permit: DispatchPermit) -> Result<String, DispatchError> {
        {
            let mut records = self.records.lock().map_err(|_| DispatchError::Uncertain)?;
            if records.contains_key(&permit.order_id) || records.len() >= 1000 {
                return Err(DispatchError::Uncertain);
            }
            records.insert(
                permit.order_id.clone(),
                ShipmentRecord {
                    owner: permit.owner,
                    amount: permit.amount,
                    receipt: permit.receipt,
                    tracking: None,
                    rejected: false,
                },
            );
        }
        #[cfg(test)]
        if matches!(self.behavior, ShippingBehavior::Reject) {
            let mut records = self.records.lock().map_err(|_| DispatchError::Uncertain)?;
            records
                .get_mut(&permit.order_id)
                .ok_or(DispatchError::Uncertain)?
                .rejected = true;
            return Err(DispatchError::Rejected);
        }
        sleep(Duration::from_millis(400)).await;
        let tracking = format!("TRK-{}", uuid::Uuid::new_v4());
        let mut records = self.records.lock().map_err(|_| DispatchError::Uncertain)?;
        let record = records
            .get_mut(&permit.order_id)
            .ok_or(DispatchError::Uncertain)?;
        record.tracking = Some(tracking.clone());
        #[cfg(test)]
        if matches!(self.behavior, ShippingBehavior::Uncertain) {
            return Err(DispatchError::Uncertain);
        }
        Ok(tracking)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn order(items: Vec<String>) -> ValidatedOrder {
        let amount = items.len() as u32 * 10;
        ValidatedOrder {
            reservation_owner: uuid::Uuid::new_v4().to_string(),
            catalog_prices: vec![10; items.len()],
            payable_amount: amount,
            request: crate::domain::OrderRequest {
                order_id: uuid::Uuid::new_v4().to_string(),
                items,
                total_amount: amount,
            },
        }
    }
    fn inventory(stock: u32) -> InventorySynapse {
        InventorySynapse::new(Arc::new(Mutex::new(HashMap::from([(
            "p".into(),
            Product {
                id: "p".into(),
                name: "fixture".into(),
                price: 10,
                stock,
            },
        )]))))
    }
    #[tokio::test]
    async fn failed_carts_do_not_partially_consume_stock() {
        for items in [
            vec!["p".into(), "p".into(), "p".into()],
            vec!["p".into(), "missing".into()],
        ] {
            let db = inventory(2);
            let result = db.call(order(items)).await;
            assert!(!matches!(result, Ok(true)));
            assert_eq!(db.inventory.lock().unwrap()["p"].stock, 2);
        }
    }
    #[tokio::test]
    async fn concurrent_carts_cannot_oversell_the_same_stock() {
        let db = inventory(2);
        let (first, second) = tokio::join!(
            db.call(order(vec!["p".into(), "p".into()])),
            db.call(order(vec!["p".into(), "p".into()]))
        );
        assert_ne!(first.unwrap(), second.unwrap());
        assert_eq!(db.inventory.lock().unwrap()["p"].stock, 0);
    }

    #[tokio::test]
    async fn release_uses_recorded_cart_and_rejects_another_owner() {
        let db = inventory(5);
        let reserved = order(vec!["p".into(), "p".into()]);
        assert!(db.call(reserved.clone()).await.unwrap());
        let mut stranger = reserved.clone();
        stranger.reservation_owner = "another-invocation".into();
        assert!(db.release(&stranger).is_err());
        assert_eq!(db.inventory.lock().unwrap()["p"].stock, 3);
        let mut altered = reserved.clone();
        altered.request.items = vec!["missing".into(); 100];
        db.release(&altered).unwrap();
        db.release(&reserved).unwrap();
        assert_eq!(db.inventory.lock().unwrap()["p"].stock, 5);
        assert!(db.call(reserved).await.is_err()); // Released IDs are not replay permission.
    }

    #[tokio::test]
    async fn pending_or_approved_payment_preserves_reservation() {
        for approved in [None, Some(true)] {
            let db = inventory(5);
            let reserved = order(vec!["p".into()]);
            assert!(db.call(reserved.clone()).await.unwrap());
            db.begin_payment(&reserved).unwrap();
            if let Some(approved) = approved {
                db.confirm_payment(&reserved, approved).unwrap();
            }
            assert!(db.release(&reserved).is_err());
            assert!(db.begin_payment(&reserved).is_err());
            assert_eq!(db.inventory.lock().unwrap()["p"].stock, 4);
        }
    }

    #[tokio::test]
    async fn failed_restoration_does_not_mutate_other_items() {
        let db = inventory(5);
        db.inventory.lock().unwrap().insert(
            "q".into(),
            Product {
                id: "q".into(),
                name: "other".into(),
                price: 10,
                stock: 5,
            },
        );
        let reserved = order(vec!["p".into(), "q".into()]);
        assert!(db.call(reserved.clone()).await.unwrap());
        db.inventory.lock().unwrap().get_mut("q").unwrap().stock = u32::MAX;
        assert!(db.release(&reserved).is_err());
        assert_eq!(db.inventory.lock().unwrap()["p"].stock, 4);
        assert!(!db.reservations.lock().unwrap()[&reserved.request.order_id].released);
    }

    #[tokio::test]
    async fn refund_binds_receipt_owner_amount_and_is_idempotent() {
        let payment = PaymentSynapse::default();
        let charged = order(vec!["p".into()]);
        assert!(payment.call(charged.clone()).await.unwrap());
        let mut stranger = charged.clone();
        stranger.reservation_owner = "other".into();
        assert!(payment.refund(&stranger).is_err());
        stranger = charged.clone();
        stranger.payable_amount += 1;
        assert!(payment.refund(&stranger).is_err());
        assert_eq!(payment.test_balance(&charged.request.order_id), (10, false));
        let first = payment.refund(&charged).unwrap();
        let second = payment.refund(&charged).unwrap();
        assert!(first.refunded && second.refunded);
        assert_eq!(first.receipt, second.receipt);
        assert!(first.receipt.is_some());
        assert_eq!(payment.test_balance(&charged.request.order_id), (0, true));
        assert!(payment.call(charged).await.is_err());
    }

    #[tokio::test]
    async fn dropped_pending_payment_cannot_be_refunded_or_replayed() {
        let payment = PaymentSynapse::default();
        let charged = order(vec!["p".into()]);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), payment.call(charged.clone()))
                .await
                .is_err()
        );
        assert!(payment.refund(&charged).is_err());
        assert!(payment.call(charged.clone()).await.is_err());
        assert_eq!(
            payment.records.lock().unwrap()[&charged.request.order_id].state,
            ReceiptState::Pending
        );
    }
}
