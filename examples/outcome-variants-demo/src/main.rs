/*!
# Outcome Variants Demo

## Purpose
Demonstrates all **5 Outcome variants** with practical use cases:
- `Outcome::Next` — Linear progression (most common)
- `Outcome::Fault` — Error path (validation failure, system error)
- `Outcome::Branch` — Conditional routing (named path + payload)
- `Outcome::Jump` — Loop / goto (re-visit a previous node)
- `Outcome::Emit` — Side-effect event (audit, notification)

## Key Concept
`Outcome<T, E>` is Ranvier's "control flow as data." Instead of hidden
middleware or implicit routing, every transition explicitly declares
what happens next. The Axon reads this declaration and acts accordingly.

## Running
```bash
cargo run -p outcome-variants-demo
```

## Prerequisites
- `hello-world` — basic Ranvier concepts
- `typed-state-tree` — typed state progression

## Import Note
This example uses workspace crate imports (`ranvier_core`, `ranvier_runtime`, etc.)
because it lives inside the Ranvier workspace. For your own projects, use:
```rust
use ranvier::prelude::*;
```
*/

use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_runtime::Axon;
use serde::{Deserialize, Serialize};

// ============================================================================
// Domain Type
// ============================================================================

/// A support ticket flowing through the processing pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Ticket {
    id: u32,
    category: String,
    priority: String,
    description: String,
    complete: bool,
}

// ============================================================================
// Scenario 1: Outcome::Next — Linear Progression
// ============================================================================
//
// WHEN TO USE:
//   The default case. The transition completed successfully and the result
//   should flow to the next node in the chain. Most transitions return Next.
//
// ANALOGY:
//   Like returning Ok(value) in a Result chain, but specifically
//   "proceed to the next step with this data."

#[derive(Clone)]
struct EnrichTicket;

#[async_trait]
impl Transition<Ticket, Ticket> for EnrichTicket {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        mut ticket: Ticket,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<Ticket, Self::Error> {
        println!("  [EnrichTicket] Adding metadata to ticket #{}", ticket.id);

        // Enrich the ticket with additional data
        ticket.description = format!("{} (enriched: auto-tagged)", ticket.description);

        // Outcome::Next — pass the enriched ticket to the next transition
        Outcome::Next(ticket)
    }
}

#[derive(Clone)]
struct FormatResponse;

#[async_trait]
impl Transition<Ticket, String> for FormatResponse {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        ticket: Ticket,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        let response = format!("Ticket #{} processed: {}", ticket.id, ticket.description);
        Outcome::Next(response)
    }
}

// ============================================================================
// Scenario 2: Outcome::Fault — Error Path
// ============================================================================
//
// WHEN TO USE:
//   When the transition encounters an error that prevents it from continuing.
//   The Axon stops the chain and returns the Fault to the caller.
//
// ANALOGY:
//   Like returning Err(e) in a Result chain, but the Axon pipeline handles it.
//
// WHEN NOT TO USE:
//   If the "error" is actually a different valid path (e.g., "user not found"
//   → redirect to registration), use Outcome::Branch instead.
//   Fault is for real errors; Branch is for business routing decisions.

#[derive(Clone)]
struct ValidateTicket;

#[async_trait]
impl Transition<Ticket, Ticket> for ValidateTicket {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        ticket: Ticket,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<Ticket, Self::Error> {
        println!("  [ValidateTicket] Validating ticket #{}...", ticket.id);

        if ticket.description.is_empty() {
            // Fault: unrecoverable validation error — stop the chain
            return Outcome::Fault("Ticket description cannot be empty".to_string());
        }

        if ticket.id == 0 {
            return Outcome::Fault("Invalid ticket ID: 0".to_string());
        }

        Outcome::Next(ticket)
    }
}

// ============================================================================
// Scenario 3: Outcome::Branch — Conditional Routing
// ============================================================================
//
// WHEN TO USE:
//   When the flow needs to take different paths based on a runtime condition.
//   Branch carries a named path (BranchId) and an optional JSON payload.
//   The caller matches on the BranchId to decide what to do next.
//
// DIFFERENCE FROM FAULT:
//   Branch is a valid business decision ("route to billing department").
//   Fault is an error ("invalid data, cannot proceed").
//
// DIFFERENCE FROM NEXT:
//   Next continues linearly to the next .then() node.
//   Branch exits the chain with a routing signal for the caller.

#[derive(Clone)]
struct RouteByCategory;

#[async_trait]
impl Transition<Ticket, Ticket> for RouteByCategory {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        ticket: Ticket,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<Ticket, Self::Error> {
        println!(
            "  [RouteByCategory] Routing ticket #{} (category: {})...",
            ticket.id, ticket.category
        );

        match ticket.category.as_str() {
            "billing" => {
                // Branch to billing department with priority info as payload
                let payload = serde_json::json!({ "priority": ticket.priority });
                Outcome::Branch("billing_dept".to_string(), Some(payload))
            }
            "technical" => Outcome::Branch("tech_support".to_string(), None),
            "general" => {
                // General tickets continue in the normal flow
                Outcome::Next(ticket)
            }
            other => Outcome::Fault(format!("Unknown category: {}", other)),
        }
    }
}

// ============================================================================
// Scenario 4: Outcome::Jump — Loop / Goto
// ============================================================================
//
// WHEN TO USE:
//   When the flow needs to return to a previous node in the Axon.
//   Jump carries a NodeId (UUID) targeting a specific node, plus optional payload.
//   Use cases: retry loops, re-validation, iterative refinement.
//
// CAUTION:
//   Use sparingly. Excessive jumps make flows hard to follow.
//   For automatic retries, consider RetryNode (ranvier-std) instead.

#[derive(Clone)]
struct CheckCompleteness;

#[async_trait]
impl Transition<Ticket, Ticket> for CheckCompleteness {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        ticket: Ticket,
        _resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<Ticket, Self::Error> {
        println!(
            "  [CheckCompleteness] Checking if ticket #{} is complete...",
            ticket.id
        );

        if !ticket.complete {
            // Track attempt count via Bus to prevent infinite loops
            let attempt = bus.get_cloned::<u32>().unwrap_or(0);

            if attempt >= 3 {
                return Outcome::Fault(
                    "Max attempts exceeded: ticket still incomplete".to_string(),
                );
            }

            bus.insert(attempt + 1);

            println!(
                "  [CheckCompleteness] Ticket incomplete (attempt {}/3). Requesting re-processing.",
                attempt + 1
            );

            // Jump: signal that we should go back to a previous node
            // In production, the UUID would reference a specific node in the Axon's schematic
            let payload = serde_json::json!({
                "reason": "incomplete",
                "attempt": attempt + 1
            });
            Outcome::Jump(uuid::Uuid::nil(), Some(payload))
        } else {
            Outcome::Next(ticket)
        }
    }
}

// ============================================================================
// Scenario 5: Outcome::Emit — Side-Effect Events
// ============================================================================
//
// WHEN TO USE:
//   When you need to signal external systems (audit logs, notifications,
//   metrics, async task triggers) as a result of processing.
//   Emit carries an event type (String) and an optional JSON payload.
//
// BEHAVIOR:
//   Emit exits the chain with the event data.
//   The caller receives the Emit result and can forward it to an event bus,
//   audit system, or notification service.
//
// TIP:
//   If you need to emit AND continue the chain, insert the event data into
//   the Bus and return Outcome::Next instead. See AuditSink for that pattern.

#[derive(Clone)]
struct EmitAuditEvent;

#[async_trait]
impl Transition<Ticket, Ticket> for EmitAuditEvent {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        ticket: Ticket,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<Ticket, Self::Error> {
        println!(
            "  [EmitAuditEvent] Emitting audit event for ticket #{}",
            ticket.id
        );

        // Emit: signal that this ticket was processed (for audit trail)
        let payload = serde_json::json!({
            "ticket_id": ticket.id,
            "action": "status_changed",
            "category": ticket.category,
            "new_status": "processed"
        });
        Outcome::Emit("ticket.processed".to_string(), Some(payload))
    }
}

// ============================================================================
// Main — Run All 5 Scenarios
// ============================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Outcome Variants Demo ===");
    println!("Outcome<T, E> has 5 variants that control the Axon flow:\n");

    // ── Scenario 1: Next (linear progression) ─────────────────────
    println!("--- Scenario 1: Outcome::Next ---");
    println!("    Linear flow: validate -> enrich -> format\n");
    {
        let pipeline = Axon::<Ticket, Ticket, String>::new("LinearFlow")
            .then(ValidateTicket)
            .then(EnrichTicket)
            .then(FormatResponse);

        let ticket = Ticket {
            id: 1001,
            category: "general".into(),
            priority: "normal".into(),
            description: "Setup instructions needed".into(),
            complete: true,
        };
        let mut bus = Bus::new();

        match pipeline.execute(ticket, &(), &mut bus).await {
            Outcome::Next(response) => println!("  Result: {}\n", response),
            other => println!("  Unexpected: {:?}\n", other),
        }
    }

    // ── Scenario 2: Fault (error path) ────────────────────────────
    println!("--- Scenario 2: Outcome::Fault ---");
    println!("    Validation fails -> chain stops -> Fault returned\n");
    {
        let pipeline = Axon::<Ticket, Ticket, String>::new("FaultFlow")
            .then(ValidateTicket)
            .then(EnrichTicket)
            .then(FormatResponse);

        let bad_ticket = Ticket {
            id: 1002,
            category: "general".into(),
            priority: "high".into(),
            description: "".into(), // empty -> triggers Fault
            complete: true,
        };
        let mut bus = Bus::new();

        match pipeline.execute(bad_ticket, &(), &mut bus).await {
            Outcome::Fault(err) => println!("  Fault caught: {}\n", err),
            other => println!("  Unexpected: {:?}\n", other),
        }
    }

    // ── Scenario 3: Branch (conditional routing) ──────────────────
    println!("--- Scenario 3: Outcome::Branch ---");
    println!("    Category-based routing: billing / tech / general\n");
    {
        let pipeline = Axon::<Ticket, Ticket, String>::new("BranchFlow")
            .then(ValidateTicket)
            .then(RouteByCategory);

        // 3a: Billing ticket -> Branch("billing_dept")
        let billing_ticket = Ticket {
            id: 2001,
            category: "billing".into(),
            priority: "urgent".into(),
            description: "Invoice discrepancy".into(),
            complete: true,
        };
        let mut bus = Bus::new();

        match pipeline.execute(billing_ticket, &(), &mut bus).await {
            Outcome::Branch(dept, payload) => {
                println!("  Branched to: {}", dept);
                if let Some(p) = payload {
                    println!("  Payload: {}", p);
                }
            }
            other => println!("  Unexpected: {:?}", other),
        }

        // 3b: General ticket -> Next (continues normally)
        let general_ticket = Ticket {
            id: 2002,
            category: "general".into(),
            priority: "low".into(),
            description: "General inquiry".into(),
            complete: true,
        };
        let mut bus2 = Bus::new();

        match pipeline.execute(general_ticket, &(), &mut bus2).await {
            Outcome::Next(ticket) => {
                println!("  General ticket #{} continued in flow", ticket.id)
            }
            other => println!("  Unexpected: {:?}", other),
        }
        println!();
    }

    // ── Scenario 4: Jump (loop / goto) ────────────────────────────
    println!("--- Scenario 4: Outcome::Jump ---");
    println!("    Incomplete ticket -> Jump (re-process signal)\n");
    {
        let pipeline = Axon::<Ticket, Ticket, String>::new("JumpFlow")
            .then(ValidateTicket)
            .then(CheckCompleteness);

        let incomplete_ticket = Ticket {
            id: 3001,
            category: "technical".into(),
            priority: "normal".into(),
            description: "Need help with configuration".into(),
            complete: false, // incomplete -> triggers Jump
        };
        let mut bus = Bus::new();

        match pipeline.execute(incomplete_ticket, &(), &mut bus).await {
            Outcome::Jump(_node_id, payload) => {
                println!("  Jump requested");
                if let Some(p) = payload {
                    println!("  Context: {}", p);
                }
                println!("  (In production, Axon would re-route to the target node)");
            }
            other => println!("  Unexpected: {:?}", other),
        }
        println!();
    }

    // ── Scenario 5: Emit (side-effect event) ──────────────────────
    println!("--- Scenario 5: Outcome::Emit ---");
    println!("    Audit event emitted for processed ticket\n");
    {
        let pipeline = Axon::<Ticket, Ticket, String>::new("EmitFlow")
            .then(ValidateTicket)
            .then(EmitAuditEvent);

        let ticket = Ticket {
            id: 4001,
            category: "billing".into(),
            priority: "normal".into(),
            description: "Payment confirmation".into(),
            complete: true,
        };
        let mut bus = Bus::new();

        match pipeline.execute(ticket, &(), &mut bus).await {
            Outcome::Emit(event_type, payload) => {
                println!("  Event emitted: {}", event_type);
                if let Some(p) = payload {
                    println!(
                        "  Payload: {}",
                        serde_json::to_string_pretty(&p).unwrap_or_default()
                    );
                }
            }
            other => println!("  Unexpected: {:?}", other),
        }
        println!();
    }

    // ── Summary ───────────────────────────────────────────────────
    println!("=== Quick Reference ===");
    println!("  Next(T)           -> Proceed to next node with value");
    println!("  Fault(E)          -> Stop chain, return error");
    println!("  Branch(id, data)  -> Stop chain, signal routing decision");
    println!("  Jump(uuid, data)  -> Stop chain, signal goto/loop");
    println!("  Emit(event, data) -> Stop chain, signal side-effect");

    Ok(())
}
