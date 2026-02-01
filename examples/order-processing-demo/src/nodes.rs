use crate::domain::OrderRequest;
use crate::synapses::{InventorySynapse, PaymentSynapse, ShippingSynapse};
use anyhow::Result;
use ranvier_core::prelude::*;
use ranvier_core::static_gen::StaticNode;
use ranvier_core::synapse::Synapse;

// --- Node 1: Validate Order ---
pub struct ValidateOrderNode {
    pub next: &'static str,
}

impl StaticNode for ValidateOrderNode {
    fn id(&self) -> &'static str {
        "validate_order"
    }
    fn kind(&self) -> NodeKind {
        NodeKind::Atom
    }
    fn next_nodes(&self) -> Vec<&'static str> {
        vec![self.next]
    }
}

impl ValidateOrderNode {
    pub async fn execute(&self, request: &OrderRequest) -> Result<Outcome<(), String>> {
        println!(
            "\x1b[1m[Node]\x1b[0m Validating Order #{}...",
            request.order_id
        );

        if request.total_amount <= 0 {
            return Ok(Outcome::Fault("Invalid amount".into()));
        }
        if request.items.is_empty() {
            return Ok(Outcome::Branch("empty_cart".into(), None));
        }

        Ok(Outcome::Next(()))
    }
}

// --- Node 2: Reserve Inventory ---
pub struct ReserveInventoryNode {
    pub synapse: InventorySynapse,
    pub next: &'static str,
}

impl StaticNode for ReserveInventoryNode {
    fn id(&self) -> &'static str {
        "reserve_inventory"
    }
    fn kind(&self) -> NodeKind {
        NodeKind::Atom
    }
    fn next_nodes(&self) -> Vec<&'static str> {
        vec![self.next]
    }
}

impl ReserveInventoryNode {
    pub async fn execute(&self, items: Vec<String>) -> Result<Outcome<Vec<String>, String>> {
        println!("\x1b[1m[Node]\x1b[0m Reserving Inventory...");

        match self.synapse.call(items.clone()).await {
            Ok(true) => Ok(Outcome::Next(items)),
            Ok(false) => Ok(Outcome::Branch(
                "out_of_stock".into(),
                Some(serde_json::to_value(items).unwrap()),
            )),
            Err(e) => Ok(Outcome::Fault(e)),
        }
    }
}

// --- Node 3: Payment ---
pub struct PaymentNode {
    pub synapse: PaymentSynapse,
    pub next: &'static str,
}

impl StaticNode for PaymentNode {
    fn id(&self) -> &'static str {
        "process_payment"
    }
    fn kind(&self) -> NodeKind {
        NodeKind::Atom
    }
    fn next_nodes(&self) -> Vec<&'static str> {
        vec![self.next]
    }
}

impl PaymentNode {
    pub async fn execute(&self, amount: u32) -> Result<Outcome<(), String>> {
        println!("\x1b[1m[Node]\x1b[0m Processing Payment...");

        match self.synapse.call(amount).await {
            Ok(true) => Ok(Outcome::Next(())),
            Ok(false) => Ok(Outcome::Branch("payment_declined".into(), None)),
            Err(e) => Ok(Outcome::Fault(e)),
        }
    }
}

// --- Node 4: Ship Order ---
pub struct ShipOrderNode {
    pub synapse: ShippingSynapse,
}

impl StaticNode for ShipOrderNode {
    fn id(&self) -> &'static str {
        "ship_order"
    }
    fn kind(&self) -> NodeKind {
        NodeKind::Egress
    }
    fn next_nodes(&self) -> Vec<&'static str> {
        vec![]
    }
}

impl ShipOrderNode {
    pub async fn execute(&self, order_id: String) -> Result<Outcome<String, String>> {
        println!("\x1b[1m[Node]\x1b[0m Shipping Order...");

        match self.synapse.call(order_id).await {
            Ok(tracking) => Ok(Outcome::Next(tracking)),
            Err(e) => Ok(Outcome::Fault(e)),
        }
    }
}
