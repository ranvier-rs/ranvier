mod domain;
mod nodes;
mod synapses;

use crate::domain::{OrderRequest, Product};
use crate::nodes::{PaymentNode, ReserveInventoryNode, ShipOrderNode, ValidateOrderNode};
use crate::synapses::{InventorySynapse, PaymentSynapse, ShippingSynapse};
use anyhow::Result;
use ranvier_core::prelude::*;
use ranvier_core::static_gen::StaticNode;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// For SSG JSON structure
#[derive(Serialize)]
struct StaticTree {
    nodes: Vec<StaticNodeView>,
}

#[derive(Serialize)]
struct StaticNodeView {
    id: String,
    kind: NodeKind,
    next: Vec<String>,
}

impl StaticNodeView {
    fn from_static<T: StaticNode>(node: &T) -> Self {
        Self {
            id: node.id().to_string(),
            kind: node.kind(),
            next: node.next_nodes().iter().map(|s| s.to_string()).collect(),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // 1. Setup Data & Dependencies
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

    // 2. Instantiate Nodes
    let validate = ValidateOrderNode {
        next: "reserve_inventory",
    };
    let reserve = ReserveInventoryNode {
        synapse: InventorySynapse {
            inventory: inventory_db.clone(),
        },
        next: "process_payment",
    };
    let payment = PaymentNode {
        synapse: PaymentSynapse,
        next: "ship_order",
    };
    let ship = ShipOrderNode {
        synapse: ShippingSynapse,
    };

    // 3. Check for SSG mode (ranvier schematic)
    // In a real app, this would be a separate binary or checking specific args/env vars.
    // However, `ranvier schematic` runs `cargo run`, capturing stdout.
    // We should print the JSON if a specific arg is present, or just print it at the end?
    // "ranvier schematic" just runs the binary. If the binary prints other things, it might be messy.
    // Ideally, "ranvier schematic" should maybe pass an env var like `RANVIER_SSG=1`.
    // But currently `ranvier schematic` just runs `cargo run -q`.
    // So if this app is interactive, it might block.
    // For this demo, let's just print the SSG JSON at the start if specific env var is set, or separate the logic.
    // OR, we can make the main function just run the workflow for a sample order, but have a "schema" subcommand.
    // But `ranvier schematic` runs `cargo run -p <pkg>`. It passes no args by default.
    // Let's print the JSON only if `RANVIER_MODE=schematic` env var is set.
    // But wait, `ranvier schematic` command implementation doesn't set env var.
    // I should probably update `ranvier schematic` to set `RANVIER_SCHEMATIC=true`.
    // FOR NOW, I will just print the JSON at the end, but formatted such that I can manually verify.
    // Actually, `ranvier schematic` implementation captures stdout. If I print logs, it messes up JSON.
    // But I used `println!` for logs in other files with colors.
    // I should use `eprintln!` for logs so that `stdout` is reserved for JSON output (if needed) or result.

    // To support `ranvier schematic`, this binary should OUTPUT JSON to stdout.
    // But since it's also a runtime demo, it should execute logic.
    // If I want both, I need a flag.
    // Let's verify `ranvier cli` implementation. It runs `cargo run`.
    // Users usually pass `--bin` or args.

    // Let's assume for this demo, if no args are passed, it runs the demo workflow (and logs to stderr).
    // If `--schematic` is passed, it prints JSON to stdout.
    // AND I need to update `ranvier cli` to pass `--schematic`? No, the CLI plan didn't say that.
    // The CLI plan said: "Run cargo run ... and capture stdout".
    // So the default behavior of the example MUST be to print JSON?
    // That prevents it from being a runnable demo app.

    // SOLUTION:
    // I will modify `ranvier` CLI (package: `ranvier-cli`) to set `RANVIER_SCHEMATIC=1`.
    // Then checking that env var here.

    // But first let's finish the code. I will assume `RANVIER_SCHEMATIC` env var.

    if std::env::var("RANVIER_SCHEMATIC").is_ok() {
        let tree = StaticTree {
            nodes: vec![
                StaticNodeView::from_static(&validate),
                StaticNodeView::from_static(&reserve),
                StaticNodeView::from_static(&payment),
                StaticNodeView::from_static(&ship),
            ],
        };
        println!("{}", serde_json::to_string_pretty(&tree).unwrap());
        return Ok(());
    }

    // --- Runtime Execution Logic ---
    // (Logs should go to stderr to avoid polluting stdout if we were strict, but here it's fine)

    println!("\n=== Order Processing Demo ===\n");

    let request = OrderRequest {
        order_id: "ORD-123".into(),
        items: vec!["p1".into(), "p2".into()],
        total_amount: 1250,
    };

    println!("Incoming Request: {:?}", request);

    // Step 1: Validate
    match validate.execute(&request).await? {
        Outcome::Next(_) => {
            // Step 2: Reserve
            match reserve.execute(request.items.clone()).await? {
                Outcome::Next(_items) => {
                    // Step 3: Payment
                    match payment.execute(request.total_amount).await? {
                        Outcome::Next(_) => {
                            // Step 4: Ship
                            match ship.execute(request.order_id).await? {
                                Outcome::Next(tracking) => {
                                    println!(
                                        "\n\x1b[32m[SUCCESS] Order Completed! Tracking: {}\x1b[0m",
                                        tracking
                                    );
                                }
                                _ => {}
                            }
                        }
                        Outcome::Branch(reason, _) => {
                            println!("\n\x1b[31m[FAILED] Payment Declined: {}\x1b[0m", reason)
                        }
                        Outcome::Fault(e) => {
                            println!("\x1b[31m[FAULT] Payment Error: {}\x1b[0m", e)
                        }
                        _ => {}
                    }
                }
                Outcome::Branch(reason, _) => {
                    println!("\n\x1b[31m[FAILED] Inventory Issue: {}\x1b[0m", reason)
                }
                Outcome::Fault(e) => println!("\x1b[31m[FAULT] Inventory Error: {}\x1b[0m", e),
                _ => {}
            }
        }
        Outcome::Branch(reason, _) => {
            println!("\n\x1b[31m[FAILED] Validation Failed: {}\x1b[0m", reason)
        }
        Outcome::Fault(e) => println!("\x1b[31m[FAULT] Validation Error: {}\x1b[0m", e),
        _ => {}
    }

    Ok(())
}
