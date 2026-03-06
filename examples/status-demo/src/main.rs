//! # Status Page Demo
//!
//! Demonstrates the Ranvier Status crate — generating static HTML status
//! pages and JSON status data for service monitoring dashboards.
//!
//! ## Run
//! ```bash
//! cargo run -p status-demo
//! ```
//!
//! ## Key Concepts
//! - `StatusData` — service health model with circuits and incidents
//! - `HealthStatus` — Operational, Degraded, PartialOutage, MajorOutage, Maintenance
//! - `StatusPageGenerator` — produces `index.html` + `status.json`
//! - `CircuitStatus` — per-circuit status with latency and error rate
//! - `Incident` / `IncidentUpdate` — timeline-based incident tracking
//!
//! ## Output
//! After running, open `./status-output/index.html` in a browser to view
//! the generated status page, or inspect `./status-output/status.json`
//! for machine-readable status data.

use ranvier_status::{
    CircuitStatus, HealthStatus, Incident, IncidentStatus, IncidentUpdate,
    StatusData, StatusPageGenerator,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Ranvier Status Page Demo ===");
    println!();

    // ── 1. Build service status data ──────────────────────────────

    let mut status = StatusData::new("Ranvier Order Service");

    // Add circuit statuses with metrics
    status.circuits.push(CircuitStatus {
        name: "UserAuth".to_string(),
        status: HealthStatus::Operational,
        latency_ms: Some(12.5),
        error_rate: Some(0.001),
        description: Some("JWT validation and session management".to_string()),
    });

    status.circuits.push(CircuitStatus {
        name: "OrderPipeline".to_string(),
        status: HealthStatus::Operational,
        latency_ms: Some(45.2),
        error_rate: Some(0.005),
        description: Some("Order validation, processing, and fulfillment".to_string()),
    });

    status.circuits.push(CircuitStatus {
        name: "PaymentGateway".to_string(),
        status: HealthStatus::Degraded,
        latency_ms: Some(320.0),
        error_rate: Some(0.03),
        description: Some("Payment processing via external gateway".to_string()),
    });

    status.circuits.push(CircuitStatus {
        name: "NotificationService".to_string(),
        status: HealthStatus::Maintenance,
        latency_ms: None,
        error_rate: None,
        description: Some("Email and SMS notifications (scheduled maintenance)".to_string()),
    });

    // Overall status reflects the worst circuit
    status.status = HealthStatus::Degraded;

    // ── 2. Add incident history ───────────────────────────────────

    let now = chrono::Utc::now();

    status.incidents.push(Incident {
        id: "INC-2026-001".to_string(),
        title: "PaymentGateway elevated latency".to_string(),
        status: IncidentStatus::Monitoring,
        affected_circuits: vec!["PaymentGateway".to_string()],
        created_at: now - chrono::Duration::hours(2),
        resolved_at: None,
        updates: vec![
            IncidentUpdate {
                timestamp: now - chrono::Duration::hours(2),
                status: IncidentStatus::Investigating,
                message: "Elevated p95 latency detected on payment processing nodes."
                    .to_string(),
            },
            IncidentUpdate {
                timestamp: now - chrono::Duration::hours(1),
                status: IncidentStatus::Identified,
                message: "Root cause identified: upstream provider rate limiting."
                    .to_string(),
            },
            IncidentUpdate {
                timestamp: now - chrono::Duration::minutes(30),
                status: IncidentStatus::Monitoring,
                message: "Mitigation deployed. Monitoring for recovery.".to_string(),
            },
        ],
    });

    status.incidents.push(Incident {
        id: "INC-2026-002".to_string(),
        title: "NotificationService scheduled maintenance".to_string(),
        status: IncidentStatus::Identified,
        affected_circuits: vec!["NotificationService".to_string()],
        created_at: now - chrono::Duration::minutes(15),
        resolved_at: None,
        updates: vec![IncidentUpdate {
            timestamp: now - chrono::Duration::minutes(15),
            status: IncidentStatus::Identified,
            message: "Planned maintenance window for email provider migration."
                .to_string(),
        }],
    });

    // ── 3. Display status summary ─────────────────────────────────

    println!(
        "Service: {} — {} {}",
        status.service_name,
        status.status.icon(),
        status.status.display_text()
    );
    println!();

    println!("Circuits:");
    for circuit in &status.circuits {
        let latency = circuit
            .latency_ms
            .map(|l| format!("{l:.1}ms"))
            .unwrap_or_else(|| "n/a".to_string());
        let error = circuit
            .error_rate
            .map(|e| format!("{:.1}%", e * 100.0))
            .unwrap_or_else(|| "n/a".to_string());
        println!(
            "  {} {} — latency: {}, errors: {}",
            circuit.status.icon(),
            circuit.name,
            latency,
            error
        );
    }
    println!();

    println!("Active incidents:");
    for incident in &status.incidents {
        println!(
            "  [{}] {} — {}",
            incident.id,
            incident.title,
            incident.status.display_text()
        );
        for update in &incident.updates {
            println!(
                "    {} [{}] {}",
                update.timestamp.format("%H:%M"),
                update.status.display_text(),
                update.message
            );
        }
    }
    println!();

    // ── 4. Generate static status page ────────────────────────────

    let output_dir = "./status-output";
    let generator = StatusPageGenerator::new(output_dir);
    let files = generator.generate(&status)?;

    println!("Generated files:");
    println!("  HTML: {}", files.html_path);
    println!("  JSON: {}", files.status_json_path);
    println!();

    // Show the JSON output
    let json = serde_json::to_string_pretty(&status)?;
    println!("status.json preview (first 500 chars):");
    println!("{}", &json[..json.len().min(500)]);
    if json.len() > 500 {
        println!("  ... ({} bytes total)", json.len());
    }

    println!();
    println!("Open {output_dir}/index.html in a browser to view the status page.");

    Ok(())
}
