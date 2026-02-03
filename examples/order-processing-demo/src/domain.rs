use serde::{Deserialize, Serialize};

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
