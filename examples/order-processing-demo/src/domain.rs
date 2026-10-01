use serde::{Deserialize, Serialize};

use crate::synapses::{InventorySynapse, PaymentSynapse, ShippingSynapse};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Product {
    pub id: String,
    pub name: String,
    pub price: u32,
    pub stock: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderRequest {
    pub order_id: String,
    pub items: Vec<String>, // Product IDs
    pub total_amount: u32,
}

// Internal handoff: client totals never reach the payment adapter directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatedOrder {
    pub request: OrderRequest,
    pub catalog_prices: Vec<u32>,
    pub payable_amount: u32,
}

pub struct OrderResources {
    pub inventory: InventorySynapse,
    pub payment: PaymentSynapse,
    pub shipping: ShippingSynapse,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OrderStatus {
    Pending,
    Validated,
    InventoryReserved,
    Paid,
    Shipped,
    Failed(String),
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id: String,
    pub status: OrderStatus,
    pub items: Vec<String>,
}
