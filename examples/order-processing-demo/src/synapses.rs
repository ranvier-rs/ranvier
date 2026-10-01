use crate::domain::Product;
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
}

#[async_trait]
impl Synapse for InventorySynapse {
    type Input = Vec<String>; // Requested Product IDs
    type Output = bool; // Success/Fail
    type Error = String;

    async fn call(&self, item_ids: Self::Input) -> Result<Self::Output, Self::Error> {
        println!(
            "\x1b[36m[Inventory]\x1b[0m Checking stock for {} items...",
            item_ids.len()
        );
        sleep(Duration::from_millis(300)).await; // Latency

        if item_ids.is_empty() || item_ids.len() > 1000 {
            return Err("Invalid reservation size".into());
        }
        let mut counts: HashMap<String, u32> = HashMap::new();
        for id in item_ids {
            *counts.entry(id).or_default() += 1;
        }
        let mut db = self
            .inventory
            .lock()
            .map_err(|_| "Inventory unavailable".to_owned())?;
        // Validate the whole cart before mutation under the same lock.
        for (id, count) in &counts {
            let product = db.get(id).ok_or_else(|| "Product not found".to_owned())?;
            if product.stock < *count {
                return Ok(false);
            }
        }
        for (id, count) in counts {
            let product = db
                .get_mut(&id)
                .ok_or_else(|| "Inventory changed".to_owned())?;
            product.stock -= count;
        }
        Ok(true)
    }
}

// --- Payment Synapse ---
pub struct PaymentSynapse;

#[async_trait]
impl Synapse for PaymentSynapse {
    type Input = u32; // Amount
    type Output = bool; // Approved/Declined
    type Error = String;

    async fn call(&self, amount: Self::Input) -> Result<Self::Output, Self::Error> {
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
    fn inventory(stock: u32) -> InventorySynapse {
        InventorySynapse {
            inventory: Arc::new(Mutex::new(HashMap::from([(
                "p".into(),
                Product {
                    id: "p".into(),
                    name: "fixture".into(),
                    price: 10,
                    stock,
                },
            )]))),
        }
    }
    #[tokio::test]
    async fn failed_carts_do_not_partially_consume_stock() {
        for items in [
            vec!["p".into(), "p".into(), "p".into()],
            vec!["p".into(), "missing".into()],
        ] {
            let db = inventory(2);
            let result = db.call(items).await;
            assert!(!matches!(result, Ok(true)));
            assert_eq!(db.inventory.lock().unwrap()["p"].stock, 2);
        }
    }
    #[tokio::test]
    async fn concurrent_carts_cannot_oversell_the_same_stock() {
        let db = inventory(2);
        let (first, second) = tokio::join!(
            db.call(vec!["p".into(), "p".into()]),
            db.call(vec!["p".into(), "p".into()])
        );
        assert_ne!(first.unwrap(), second.unwrap());
        assert_eq!(db.inventory.lock().unwrap()["p"].stock, 0);
    }
}
