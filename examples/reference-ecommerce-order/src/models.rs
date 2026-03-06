use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ORDER_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderStatus {
    Pending,
    Paid,
    Reserved,
    Shipped,
    FailedPayment,
    FailedInventory,
    FailedShipping,
    Compensated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderItem {
    pub product_id: String,
    pub quantity: u32,
    pub unit_price: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id: u64,
    pub tenant_id: String,
    pub customer_id: String,
    pub items: Vec<OrderItem>,
    pub total: f64,
    pub status: OrderStatus,
    pub payment_id: Option<String>,
    pub shipping_id: Option<String>,
}

impl Order {
    pub fn new(tenant_id: String, customer_id: String, items: Vec<OrderItem>) -> Self {
        let total = items.iter().map(|i| i.unit_price * i.quantity as f64).sum();
        Self {
            id: NEXT_ORDER_ID.fetch_add(1, Ordering::SeqCst),
            tenant_id,
            customer_id,
            items,
            total,
            status: OrderStatus::Pending,
            payment_id: None,
            shipping_id: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateOrderRequest {
    pub customer_id: String,
    pub items: Vec<OrderItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct PaymentResult {
    pub payment_id: String,
    pub amount: f64,
    pub success: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct InventoryReservation {
    pub reservation_id: String,
    pub items: Vec<(String, u32)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct ShippingInfo {
    pub shipping_id: String,
    pub estimated_days: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryRecord {
    pub product_id: String,
    pub available: u32,
}
