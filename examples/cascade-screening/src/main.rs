/*!
# Cascade Screening Pattern

## Purpose
Demonstrates the **cascade screening pattern** using Ranvier's Axon pipeline.
A subject passes through multiple sequential screening stages, where each stage
can reject (Fault) or pass through (Next) to the next screen.

## Pattern: Cascade Screening (Sequential Filter Pipeline)
Each transition acts as an independent screen. Early rejection short-circuits
the pipeline — no further screens are evaluated. This is ideal for compliance,
validation, and eligibility checks where fail-fast behavior saves resources.

## Applied Domain: AML/KYC Compliance
Four screening stages: sanctions list → PEP check → risk scoring → document verification.

## Key Concepts
- **Outcome::Next**: Subject passed this screen
- **Outcome::Fault**: Subject rejected at this screen (fail-fast)
- **Outcome::Emit**: Audit event for compliance trail

## Running
```bash
cargo run -p cascade-screening
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
struct Applicant {
    id: String,
    name: String,
    country: String,
    risk_score: u32,
    has_documents: bool,
    is_pep: bool,
    screens_passed: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ScreeningResult {
    applicant_id: String,
    approved: bool,
    screens_passed: Vec<String>,
    rejection_reason: Option<String>,
}

// ============================================================================
// Screen 1: Sanctions List Check
// ============================================================================

#[derive(Clone)]
struct SanctionsCheck;

#[async_trait]
impl Transition<Applicant, Applicant> for SanctionsCheck {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        mut applicant: Applicant,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<Applicant, Self::Error> {
        println!("  [Screen 1: Sanctions] Checking {} against sanctions list...", applicant.name);

        // Simulate: sanctioned countries
        let sanctioned = ["NK", "SY"];
        if sanctioned.contains(&applicant.country.as_str()) {
            return Outcome::Fault(format!(
                "REJECTED at Screen 1: {} is from sanctioned country {}",
                applicant.name, applicant.country
            ));
        }

        applicant.screens_passed.push("sanctions".to_string());
        println!("  [Screen 1: Sanctions] ✓ Passed");
        Outcome::Next(applicant)
    }
}

// ============================================================================
// Screen 2: PEP (Politically Exposed Person) Check
// ============================================================================

#[derive(Clone)]
struct PepCheck;

#[async_trait]
impl Transition<Applicant, Applicant> for PepCheck {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        mut applicant: Applicant,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<Applicant, Self::Error> {
        println!("  [Screen 2: PEP] Checking if {} is a PEP...", applicant.name);

        if applicant.is_pep {
            return Outcome::Fault(format!(
                "REJECTED at Screen 2: {} flagged as Politically Exposed Person",
                applicant.name
            ));
        }

        applicant.screens_passed.push("pep".to_string());
        println!("  [Screen 2: PEP] ✓ Passed");
        Outcome::Next(applicant)
    }
}

// ============================================================================
// Screen 3: Risk Score Evaluation
// ============================================================================

#[derive(Clone)]
struct RiskScoring;

#[async_trait]
impl Transition<Applicant, Applicant> for RiskScoring {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        mut applicant: Applicant,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<Applicant, Self::Error> {
        println!("  [Screen 3: Risk] Evaluating risk score for {} (score: {})...",
            applicant.name, applicant.risk_score);

        if applicant.risk_score > 80 {
            return Outcome::Fault(format!(
                "REJECTED at Screen 3: Risk score {} exceeds threshold (80)",
                applicant.risk_score
            ));
        }

        applicant.screens_passed.push("risk_scoring".to_string());
        println!("  [Screen 3: Risk] ✓ Passed (score: {} ≤ 80)", applicant.risk_score);
        Outcome::Next(applicant)
    }
}

// ============================================================================
// Screen 4: Document Verification
// ============================================================================

#[derive(Clone)]
struct DocumentVerification;

#[async_trait]
impl Transition<Applicant, ScreeningResult> for DocumentVerification {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        mut applicant: Applicant,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<ScreeningResult, Self::Error> {
        println!("  [Screen 4: Docs] Verifying documents for {}...", applicant.name);

        if !applicant.has_documents {
            return Outcome::Fault(format!(
                "REJECTED at Screen 4: {} has missing required documents",
                applicant.name
            ));
        }

        applicant.screens_passed.push("documents".to_string());
        println!("  [Screen 4: Docs] ✓ Passed");

        Outcome::Next(ScreeningResult {
            applicant_id: applicant.id,
            approved: true,
            screens_passed: applicant.screens_passed,
            rejection_reason: None,
        })
    }
}

// ============================================================================
// Main — Demonstrate Cascade Screening
// ============================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Cascade Screening Pattern ===");
    println!("Pattern: Sequential filter pipeline with fail-fast rejection");
    println!("Domain example: AML/KYC compliance screening\n");

    let screening = Axon::<Applicant, Applicant, String>::new("ComplianceScreening")
        .then(SanctionsCheck)
        .then(PepCheck)
        .then(RiskScoring)
        .then(DocumentVerification);

    if screening.maybe_export_and_exit()? {
        return Ok(());
    }

    // ── Scenario 1: All screens pass ─────────────────────────────
    println!("--- Scenario 1: Clean applicant (all screens pass) ---\n");
    {
        let applicant = Applicant {
            id: "APP-001".to_string(),
            name: "Alice Chen".to_string(),
            country: "US".to_string(),
            risk_score: 25,
            has_documents: true,
            is_pep: false,
            screens_passed: vec![],
        };
        let mut bus = Bus::new();

        match screening.execute(applicant, &(), &mut bus).await {
            Outcome::Next(result) => {
                println!("\n  APPROVED: {} passed {} screens: {:?}",
                    result.applicant_id, result.screens_passed.len(), result.screens_passed);
            }
            Outcome::Fault(err) => println!("\n  {}", err),
            other => println!("\n  Unexpected: {:?}", other),
        }
    }

    // ── Scenario 2: Rejected at Screen 1 (sanctions) ────────────
    println!("\n--- Scenario 2: Rejected at sanctions check (Screen 1) ---\n");
    {
        let applicant = Applicant {
            id: "APP-002".to_string(),
            name: "Bob Kim".to_string(),
            country: "NK".to_string(), // sanctioned country
            risk_score: 10,
            has_documents: true,
            is_pep: false,
            screens_passed: vec![],
        };
        let mut bus = Bus::new();

        match screening.execute(applicant, &(), &mut bus).await {
            Outcome::Fault(err) => {
                println!("  {}", err);
                println!("  (Screens 2-4 were never evaluated — fail-fast)");
            }
            other => println!("\n  Unexpected: {:?}", other),
        }
    }

    // ── Scenario 3: Rejected at Screen 3 (risk score) ───────────
    println!("\n--- Scenario 3: Passes first two, rejected at risk scoring ---\n");
    {
        let applicant = Applicant {
            id: "APP-003".to_string(),
            name: "Carol Zhang".to_string(),
            country: "DE".to_string(),
            risk_score: 92, // exceeds threshold
            has_documents: true,
            is_pep: false,
            screens_passed: vec![],
        };
        let mut bus = Bus::new();

        match screening.execute(applicant, &(), &mut bus).await {
            Outcome::Fault(err) => {
                println!("  {}", err);
                println!("  (Screen 4 was never evaluated — fail-fast)");
            }
            other => println!("\n  Unexpected: {:?}", other),
        }
    }

    // ── Summary ──────────────────────────────────────────────────
    println!("\n=== Cascade Screening Summary ===");
    println!("  1. Each transition = one independent screen");
    println!("  2. Outcome::Next = passed, proceed to next screen");
    println!("  3. Outcome::Fault = rejected, short-circuit (fail-fast)");
    println!("  4. Order matters: cheapest/fastest checks first");
    println!("  5. Each screen is independently testable and replaceable");

    Ok(())
}
