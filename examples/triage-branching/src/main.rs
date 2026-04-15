/*!
# Triage Branching Pattern

## Purpose
Demonstrates the **multi-stage branching pattern** using Ranvier's Axon pipeline
and `Outcome::Branch`. At each stage, the pipeline can route the subject to
different named paths based on classification results.

## Pattern: Multi-stage Classification with Branch Routing
Each transition evaluates the subject and either passes it forward (Next) or
routes it to a named branch (Branch). The caller matches on branch names to
determine the next action.

## Applied Domain: Patient Triage
A patient arrives → vitals assessment → severity classification → department routing.

## Key Concepts
- **Outcome::Branch**: Named routing decision with optional payload
- **Outcome::Next**: Continue to next classification stage
- **Outcome::Fault**: Invalid input, cannot classify

## Running
```bash
cargo run -p triage-branching
```

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
// Domain Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Patient {
    id: String,
    name: String,
    age: u32,
    heart_rate: u32,     // bpm
    blood_pressure: u32, // systolic mmHg
    temperature: f64,    // Celsius
    chief_complaint: String,
    severity: Option<Severity>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum Severity {
    Critical,  // Immediate attention
    Urgent,    // Within 15 minutes
    Standard,  // Within 1 hour
    NonUrgent, // Walk-in
}

// ============================================================================
// Stage 1: Vitals Assessment
// ============================================================================

#[derive(Clone)]
struct AssessVitals;

#[async_trait]
impl Transition<Patient, Patient> for AssessVitals {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        mut patient: Patient,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<Patient, Self::Error> {
        println!(
            "  [AssessVitals] Patient {}: HR={}, BP={}, Temp={:.1}°C",
            patient.name, patient.heart_rate, patient.blood_pressure, patient.temperature
        );

        // Validate vitals
        if patient.heart_rate == 0 {
            return Outcome::Fault(format!(
                "Invalid vitals for patient {}: heart rate is 0",
                patient.id
            ));
        }

        // Critical vitals → immediate branch
        let critical = patient.heart_rate > 150
            || patient.heart_rate < 40
            || patient.blood_pressure > 200
            || patient.blood_pressure < 70
            || patient.temperature > 40.0;

        if critical {
            patient.severity = Some(Severity::Critical);
            println!("  [AssessVitals] CRITICAL vitals detected!");
            let payload = serde_json::json!({
                "patient_id": patient.id,
                "reason": "critical_vitals",
                "heart_rate": patient.heart_rate,
                "blood_pressure": patient.blood_pressure,
                "temperature": patient.temperature,
            });
            return Outcome::Branch("emergency".to_string(), Some(payload));
        }

        println!("  [AssessVitals] Vitals within non-critical range");
        Outcome::Next(patient)
    }
}

// ============================================================================
// Stage 2: Severity Classification
// ============================================================================

#[derive(Clone)]
struct ClassifySeverity;

#[async_trait]
impl Transition<Patient, Patient> for ClassifySeverity {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        mut patient: Patient,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<Patient, Self::Error> {
        println!(
            "  [ClassifySeverity] Evaluating complaint: \"{}\"",
            patient.chief_complaint
        );

        // Simple keyword-based classification (in production: clinical rules engine)
        let complaint_lower = patient.chief_complaint.to_lowercase();

        let severity = if complaint_lower.contains("chest pain")
            || complaint_lower.contains("breathing")
        {
            Severity::Urgent
        } else if complaint_lower.contains("fracture") || complaint_lower.contains("severe pain") {
            Severity::Urgent
        } else if complaint_lower.contains("fever") || complaint_lower.contains("infection") {
            Severity::Standard
        } else if complaint_lower.contains("headache") || complaint_lower.contains("cold") {
            Severity::NonUrgent
        } else {
            Severity::Standard // default
        };

        println!("  [ClassifySeverity] Severity: {:?}", severity);
        patient.severity = Some(severity);
        Outcome::Next(patient)
    }
}

// ============================================================================
// Stage 3: Department Routing
// ============================================================================

#[derive(Clone)]
struct RouteToDepartment;

#[async_trait]
impl Transition<Patient, Patient> for RouteToDepartment {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        patient: Patient,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<Patient, Self::Error> {
        let severity = patient
            .severity
            .as_ref()
            .expect("severity must be set by ClassifySeverity");
        println!(
            "  [RouteToDepartment] Routing patient {} ({:?})...",
            patient.name, severity
        );

        let (branch, dept) = match severity {
            Severity::Critical => ("emergency", "Emergency Department"),
            Severity::Urgent => ("urgent_care", "Urgent Care"),
            Severity::Standard => ("general_ward", "General Ward"),
            Severity::NonUrgent => ("outpatient", "Outpatient Clinic"),
        };

        println!("  [RouteToDepartment] → {}", dept);

        let payload = serde_json::json!({
            "patient_id": patient.id,
            "patient_name": patient.name,
            "department": dept,
            "severity": format!("{:?}", severity),
        });
        Outcome::Branch(branch.to_string(), Some(payload))
    }
}

// ============================================================================
// Main — Demonstrate Triage Branching
// ============================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Triage Branching Pattern ===");
    println!("Pattern: Multi-stage classification with Outcome::Branch routing");
    println!("Domain example: Patient triage in emergency department\n");

    let triage = Axon::<Patient, Patient, String>::new("PatientTriage")
        .then(AssessVitals)
        .then(ClassifySeverity)
        .then(RouteToDepartment);

    if triage.maybe_export_and_exit()? {
        return Ok(());
    }

    let patients = vec![
        (
            "Normal headache → Outpatient",
            Patient {
                id: "PT-001".to_string(),
                name: "Alice".to_string(),
                age: 32,
                heart_rate: 72,
                blood_pressure: 120,
                temperature: 36.8,
                chief_complaint: "Mild headache for 2 days".to_string(),
                severity: None,
            },
        ),
        (
            "Chest pain → Urgent Care",
            Patient {
                id: "PT-002".to_string(),
                name: "Bob".to_string(),
                age: 55,
                heart_rate: 95,
                blood_pressure: 145,
                temperature: 37.0,
                chief_complaint: "Chest pain and shortness of breathing".to_string(),
                severity: None,
            },
        ),
        (
            "Critical vitals → Emergency (bypasses classification)",
            Patient {
                id: "PT-003".to_string(),
                name: "Carol".to_string(),
                age: 68,
                heart_rate: 160,     // critical HR
                blood_pressure: 210, // critical BP
                temperature: 39.5,
                chief_complaint: "Dizziness and confusion".to_string(),
                severity: None,
            },
        ),
        (
            "Fever → General Ward",
            Patient {
                id: "PT-004".to_string(),
                name: "David".to_string(),
                age: 28,
                heart_rate: 88,
                blood_pressure: 118,
                temperature: 38.5,
                chief_complaint: "High fever and possible infection".to_string(),
                severity: None,
            },
        ),
    ];

    for (i, (label, patient)) in patients.into_iter().enumerate() {
        println!("--- Scenario {}: {} ---\n", i + 1, label);
        let mut bus = Bus::new();

        match triage.execute(patient, &(), &mut bus).await {
            Outcome::Branch(dept, payload) => {
                println!("\n  Routed to: {}", dept);
                if let Some(p) = payload {
                    println!(
                        "  Details: {}",
                        serde_json::to_string_pretty(&p).unwrap_or_default()
                    );
                }
            }
            Outcome::Next(p) => {
                println!("\n  Completed triage for {} ({:?})", p.name, p.severity);
            }
            Outcome::Fault(err) => {
                println!("\n  Triage error: {}", err);
            }
            other => println!("\n  Unexpected: {:?}", other),
        }
        println!();
    }

    // ── Summary ──────────────────────────────────────────────────
    println!("=== Triage Branching Summary ===");
    println!("  1. Outcome::Branch carries a named route + JSON payload");
    println!("  2. Critical conditions can short-circuit early stages");
    println!("  3. Each classification stage adds information progressively");
    println!("  4. The caller matches on branch names to dispatch actions");
    println!("  5. Same pattern works for: ticket routing, loan approval, content moderation");

    Ok(())
}
