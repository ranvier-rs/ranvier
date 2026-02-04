mod domain;
mod nodes;
mod synapses;

use crate::domain::{OrderRequest, OrderResources, Product};
use crate::nodes::{ProcessPayment, ReserveInventory, ShipOrder, ValidateOrder};
use crate::synapses::{InventorySynapse, PaymentSynapse, ShippingSynapse};
use anyhow::Result;
use ranvier_core::prelude::*;
use ranvier_runtime::Axon;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[tokio::main]
async fn main() -> Result<()> {
    // 1) Setup data and dependencies
    let products = HashMap::from([
        (
            "p1".to_string(),
            Product {
                id: "p1".into(),
                name: "Laptop".into(),
                price: 1200,
                stock: 5,
            },
        ),
        (
            "p2".to_string(),
            Product {
                id: "p2".into(),
                name: "Mouse".into(),
                price: 50,
                stock: 2,
            },
        ),
    ]);
    let inventory_db = Arc::new(Mutex::new(products));
    let resources = OrderResources {
        inventory: InventorySynapse {
            inventory: inventory_db,
        },
        payment: PaymentSynapse,
        shipping: ShippingSynapse,
    };

    // 2) Build Axon execution chain
    let axon = Axon::<OrderRequest, OrderRequest, String, OrderResources>::new("OrderProcessing")
        .then(ValidateOrder)
        .then(ReserveInventory)
        .then(ProcessPayment)
        .then(ShipOrder);

    // 3) Schematic mode for CLI integration
    if std::env::var("RANVIER_SCHEMATIC").is_ok() {
        println!("{}", serde_json::to_string_pretty(axon.schematic())?);
        return Ok(());
    }

    println!("\n=== Order Processing Demo ===\n");

    let success_case = OrderRequest {
        order_id: "ORD-123".into(),
        items: vec!["p1".into()],
        total_amount: 950,
    };
    run_order_flow("Success Case", success_case, &axon, &resources).await?;

    let payment_declined_case = OrderRequest {
        order_id: "ORD-124".into(),
        items: vec!["p1".into()],
        total_amount: 1250,
    };
    run_order_flow("Payment Declined Case", payment_declined_case, &axon, &resources).await?;

    let out_of_stock_case = OrderRequest {
        order_id: "ORD-125".into(),
        items: vec!["p2".into(), "p2".into(), "p2".into()],
        total_amount: 150,
    };
    run_order_flow("Out Of Stock Case", out_of_stock_case, &axon, &resources).await?;

    Ok(())
}

async fn run_order_flow(
    title: &str,
    request: OrderRequest,
    axon: &Axon<OrderRequest, String, String, OrderResources>,
    resources: &OrderResources,
) -> Result<()> {
    println!("--- {} ---", title);
    println!("Incoming Request: {:?}", request);
    let mut bus = Bus::new();

    match axon.execute(request, resources, &mut bus).await {
        Outcome::Next(tracking) => println!(
            "\n\x1b[32m[SUCCESS] Order Completed! Tracking: {}\x1b[0m",
            tracking
        ),
        Outcome::Branch(reason, _) => println!("\n\x1b[31m[FAILED] Branch: {}\x1b[0m", reason),
        Outcome::Fault(e) => println!("\n\x1b[31m[FAULT] Error: {}\x1b[0m", e),
        Outcome::Jump(id, _) => println!("\n\x1b[33m[JUMP] {}\x1b[0m", id),
        Outcome::Emit(event, _) => println!("\n\x1b[34m[EMIT] {}\x1b[0m", event),
    };

    println!();
    Ok(())
}
