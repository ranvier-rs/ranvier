use crate::models::{InventoryRecord, Order, OrderStatus};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// In-memory store for orders and inventory. Thread-safe via Arc<Mutex>.
#[derive(Debug, Clone)]
pub struct AppStore {
    pub orders: Arc<Mutex<HashMap<u64, Order>>>,
    pub inventory: Arc<Mutex<HashMap<String, u32>>>,
    pub refunded_payments: Arc<Mutex<Vec<String>>>,
}

impl AppStore {
    pub fn new() -> Self {
        let mut inventory = HashMap::new();
        // Seed some inventory
        inventory.insert("WIDGET-001".to_string(), 100);
        inventory.insert("GADGET-002".to_string(), 50);
        inventory.insert("GIZMO-003".to_string(), 10);
        inventory.insert("DOOHICKEY-004".to_string(), 0); // out of stock

        Self {
            orders: Arc::new(Mutex::new(HashMap::new())),
            inventory: Arc::new(Mutex::new(inventory)),
            refunded_payments: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn save_order(&self, order: &Order) {
        self.orders.lock().unwrap().insert(order.id, order.clone());
    }

    pub fn get_order(&self, id: u64) -> Option<Order> {
        self.orders.lock().unwrap().get(&id).cloned()
    }

    pub fn list_orders(&self, tenant_id: &str) -> Vec<Order> {
        self.orders
            .lock()
            .unwrap()
            .values()
            .filter(|o| o.tenant_id == tenant_id)
            .cloned()
            .collect()
    }

    pub fn update_order_status(&self, id: u64, status: OrderStatus) {
        if let Some(order) = self.orders.lock().unwrap().get_mut(&id) {
            order.status = status;
        }
    }

    pub fn reserve_inventory(&self, product_id: &str, quantity: u32) -> bool {
        let mut inv = self.inventory.lock().unwrap();
        if let Some(stock) = inv.get_mut(product_id) {
            if *stock >= quantity {
                *stock -= quantity;
                return true;
            }
        }
        false
    }

    pub fn release_inventory(&self, product_id: &str, quantity: u32) {
        let mut inv = self.inventory.lock().unwrap();
        *inv.entry(product_id.to_string()).or_insert(0) += quantity;
    }

    pub fn get_inventory(&self) -> Vec<InventoryRecord> {
        self.inventory
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| InventoryRecord {
                product_id: k.clone(),
                available: *v,
            })
            .collect()
    }

    pub fn record_refund(&self, payment_id: &str) {
        self.refunded_payments
            .lock()
            .unwrap()
            .push(payment_id.to_string());
    }
}
