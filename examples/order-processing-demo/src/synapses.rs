use crate::domain::Product;
use anyhow::Result;
use async_trait::async_trait;
use ranvier_core::synapse::Synapse;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::time::{sleep, Duration};

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

        let mut db = self.inventory.lock().unwrap();
        for id in item_ids {
            if let Some(product) = db.get_mut(&id) {
                if product.stock > 0 {
                    product.stock -= 1;
                    println!(
                        "\x1b[36m[Inventory]\x1b[0m Reserved '{}'. Remaining: {}",
                        product.name, product.stock
                    );
                } else {
                    println!("\x1b[31m[Inventory]\x1b[0m Out of stock: {}", product.name);
                    return Ok(false);
                }
            } else {
                return Err(format!("Product not found: {}", id));
            }
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
