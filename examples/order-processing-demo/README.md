# Order Processing Demo

A high-quality demonstration of the **Ranvier Framework**, showcasing a complete business workflow with domain logic, integration points (Synapses), and static analysis (SSG).

## Scenario
This app simulates an e-commerce order processing flow:
1.  **Validate Order**: Checks amount and items.
2.  **Reserve Inventory**: Connects to a mock Inventory Database (Synapse).
3.  **Process Payment**: Connects to a mock Payment Gateway (Synapse).
4.  **Ship Order**: Generates tracking number via Shipping Service (Synapse).

## Key Features

- **Axon-Centered Runtime**: Uses `Axon::new().then(...)` as the executable decision path.
- **Synapse Integration**: Shows how to cleanly abstract side-effects (DB, API) using the `Synapse` trait.
- **Type-Safe Outcomes**: Uses `Outcome::Next`, `Branch`, and `Fault` to model business logic.
- **Static State Generation (SSG)**: Can extract its own topology graph without running the workflow.

## How to Run

### 1. Run the Runtime Logic
Executes three finite scenarios and prints logs:
1. Success
2. Payment declined
3. Out of stock

```bash
cargo run -p order-processing-demo
```

**Output:**
```text
=== Order Processing Demo ===

Incoming Request: OrderRequest { ... }
[Node] Validating Order #ORD-123...
[Node] Reserving Inventory...
[Inventory] Checking stock for 2 items...
[Inventory] Reserved 'Laptop'. Remaining: 4
...
[SUCCESS] Order Completed! Tracking: TRK-1234...
```

### 2. Extract Schematic (SSG)
Uses `RANVIER_SCHEMATIC=1` to generate Ranvier `Schematic` JSON.

```bash
# Using Ranvier CLI
ranvier schematic order-processing-demo

# Or manually
RANVIER_SCHEMATIC=1 cargo run -q -p order-processing-demo
```

**Output:**
```json
{
  "schema_version": "1.0",
  "id": "<uuid>",
  "name": "OrderProcessing",
  "nodes": [...],
  "edges": [...]
}
```

## Project Structure

- `domain.rs`: Pure domain types (`Order`, `Product`).
- `synapses.rs`: Integration implementations (`InventorySynapse` etc.).
- `nodes.rs`: Unit logic implementing `StaticNode` regarding flow connections.
- `main.rs`: Wiring and execution.
