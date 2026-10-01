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
}
struct Reservation {
    owner: String,
    counts: HashMap<String, u32>,
    amount: u32,
    released: bool,
    payment: PaymentState,
}

impl InventorySynapse {
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
            },
        );
        Ok(true)
    }
}

// --- Payment Synapse ---
#[derive(Default)]
pub struct PaymentSynapse {
    // Local fixture evidence only; not a durable payment ledger.
    pub attempted_amounts: Mutex<Vec<u32>>,
}

#[async_trait]
impl Synapse for PaymentSynapse {
    type Input = u32; // Amount
    type Output = bool; // Approved/Declined
    type Error = String;

    async fn call(&self, amount: Self::Input) -> Result<Self::Output, Self::Error> {
        self.attempted_amounts
            .lock()
            .map_err(|_| "Payment unavailable".to_owned())?
            .push(amount);
        println!(
            "\x1b[33m[Payment]\x1b[0m Processing transaction: ${}",
            amount
        );
        sleep(Duration::from_millis(500)).await;

        if amount > 1000 {
            println!("\x1b[31m[Payment]\x1b[0m Amount too high (Declined).");
            Ok(false)
        } else {
            println!("\x1b[32m[Payment]\x1b[0m Transaction Approved.");
            Ok(true)
        }
    }
}

// --- Shipping Synapse ---
pub struct ShippingSynapse;

#[async_trait]
impl Synapse for ShippingSynapse {
    type Input = String; // Order ID
    type Output = String; // Tracking Number
    type Error = String;

    async fn call(&self, order_id: Self::Input) -> Result<Self::Output, Self::Error> {
        println!(
            "\x1b[35m[Shipping]\x1b[0m Dispatching Order #{}...",
            order_id
        );
        sleep(Duration::from_millis(400)).await;

        let tracking = format!("TRK-{}", uuid::Uuid::new_v4());
        println!("\x1b[35m[Shipping]\x1b[0m Shipped! Tracking: {}", tracking);
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
}
