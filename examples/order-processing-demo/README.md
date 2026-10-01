# Order Processing Demo

A local simulation of a typed order flow with integration adapters (Synapses)
and Schematic extraction. It does not implement a production order ledger.

## Scenario
This app simulates an e-commerce order processing flow:
1.  **Validate Order**: Computes the catalog total with checked integer addition
    and rejects a mismatched client total. Each repeated product ID is one unit.
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
[Inventory] Checking stock for 1 items...
...
[SUCCESS] Order Completed! Tracking: TRK-1234...
```

### 2. Extract Schematic (SSG)
Uses `--schematic` mode to generate Ranvier `Schematic` JSON.

```bash
# Using Ranvier CLI
ranvier schematic order-processing-demo

# Or manually
cargo run -q -p order-processing-demo -- --schematic
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
- `nodes.rs`: Named typed `Transition` implementations.
- `main.rs`: Wiring and execution.

## Authority and recovery limits

Amounts use whole currency units in this example; there is no fractional price,
discount or tax rounding. A nonempty cart has at most 1000 units; zero catalog
prices and u32 sum overflow fail before reservation/payment. `ValidatedOrder`
carries a per-line price snapshot and the computed payable amount. Reservation
rechecks every price and total under the same lock as stock validation/deduction.
An unchanged aggregate total cannot hide changed line prices. Once reserved,
the quoted amount is honored for that invocation; no live repricing at payment.

This is a process-local snapshot, not a persisted catalog revision or a trusted
resume authority. The internal serialized type alone does not authorize replay
or bypassing validation. Real catalog versions, ledger ownership, authentication,
and idempotency require separate integration work.

Payment records local attempts and owned simulated receipts with settled balance.
Both payment and shipping are simulated; these are not remote payment receipts.
Confirmed payment decline now releases the recorded reservation before returning
the decline Branch. Runtime automatic Saga compensation runs on Fault, not
Branch; the reservation step also has an explicit release hook for faults.
The ledger binds each order ID to a fresh invocation owner and recorded item
counts. Release uses these counts, checks all restorations before mutation, and
marks completion so repeated release cannot add stock twice. Order IDs, including
released IDs, cannot be reserved again in this ledger. The finite demo ledger
retains at most 1000 records and then requires reconciliation.

Payment starts in Pending before the simulated call. Pending or Approved payment
prevents release until refund is confirmed. The ProcessPayment compensation hook
consults the actual local payment record and returns a confirmation bound to
order, invocation owner, amount and receipt; only then can the reservation become
Refunded and be released. Repeated refund returns the same receipt and cannot
refund twice. Failed, panicked or uncertain refund retains the outstanding amount
and reservation and returns incomplete cleanup, requiring reconciliation.

Tests exercise a pure downstream rejection after charge with the real refund
hook, including injected Fault/panic/uncertainty. Shipping itself currently always
succeeds and has no reversal hook; real dispatch failure/ambiguity and terminal
fulfillment records remain OPEN. A timeout test drops the local pending payment
future and verifies replay/refund rejection, but does not prove process crash or
whole-runtime cancellation recovery. Real gateway/database recovery is unproven.

Use one OrderResources/InventorySynapse instance for this process-local fixture.
Creating separate InventorySynapse instances over the same inventory Arc creates
separate reservation ledgers; it is not a supported shared authority. Persisted
atomic stock/ledger transactions, tenant ownership, catalog revision history,
restart recovery and bounded durable retention require further implementation.

The local ExecutionReport policy rejects persisted/uncertain reports and
incomplete cleanup before domain mapping. It never authorizes automatic retry.
