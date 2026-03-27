/*!
# Eligibility Rule Chain Pattern

## Purpose
Demonstrates the **rule chain pattern** using Ranvier's Axon pipeline.
A subject is evaluated against a sequence of independent rules. Each rule
can either pass (Next) or reject (Fault). Rules are composable and reorderable.

## Pattern: Sequential Rule Evaluation Chain
Each transition represents an independent rule. The chain short-circuits on the
first rejection. Rules don't depend on each other, so they can be reordered,
added, or removed without affecting other rules.

## Applied Domain: Welfare Eligibility
An applicant is evaluated against: age requirement → income threshold → residency
requirement → benefit cap check.

## Key Concepts
- **Each transition = one rule**: independently testable and deployable
- **Outcome::Fault**: Rule violation — chain stops with rejection reason
- **Outcome::Next**: Rule passed — proceed to next rule
- **Bus**: Carries accumulated rule evaluation context

## Running
```bash
cargo run -p eligibility-rule-chain
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
    age: u32,
    annual_income: f64,
    residency_years: u32,
    current_benefits: Vec<String>,
    rules_passed: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EligibilityResult {
    applicant_id: String,
    eligible: bool,
    rules_passed: Vec<String>,
    approved_benefit: Option<String>,
    rejection_reason: Option<String>,
}

// ============================================================================
// Rule 1: Age Requirement (18-65)
// ============================================================================

#[derive(Clone)]
struct AgeRule;

#[async_trait]
impl Transition<Applicant, Applicant> for AgeRule {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        mut applicant: Applicant,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<Applicant, Self::Error> {
        println!("  [Rule: Age] Checking age {} for {}...", applicant.age, applicant.name);

        if applicant.age < 18 {
            return Outcome::Fault(format!(
                "Age rule: {} is {} years old (minimum: 18)",
                applicant.name, applicant.age
            ));
        }
        if applicant.age > 65 {
            return Outcome::Fault(format!(
                "Age rule: {} is {} years old (maximum: 65, see senior program)",
                applicant.name, applicant.age
            ));
        }

        applicant.rules_passed.push("age".to_string());
        println!("  [Rule: Age] ✓ Passed (age {})", applicant.age);
        Outcome::Next(applicant)
    }
}

// ============================================================================
// Rule 2: Income Threshold (< $50,000)
// ============================================================================

#[derive(Clone)]
struct IncomeRule;

#[async_trait]
impl Transition<Applicant, Applicant> for IncomeRule {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        mut applicant: Applicant,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<Applicant, Self::Error> {
        println!("  [Rule: Income] Checking income ${:.0} for {}...",
            applicant.annual_income, applicant.name);

        let threshold = 50_000.0;
        if applicant.annual_income > threshold {
            return Outcome::Fault(format!(
                "Income rule: ${:.0} exceeds threshold ${:.0}",
                applicant.annual_income, threshold
            ));
        }

        applicant.rules_passed.push("income".to_string());
        println!("  [Rule: Income] ✓ Passed (${:.0} ≤ ${:.0})",
            applicant.annual_income, threshold);
        Outcome::Next(applicant)
    }
}

// ============================================================================
// Rule 3: Residency Requirement (>= 2 years)
// ============================================================================

#[derive(Clone)]
struct ResidencyRule;

#[async_trait]
impl Transition<Applicant, Applicant> for ResidencyRule {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        mut applicant: Applicant,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<Applicant, Self::Error> {
        println!("  [Rule: Residency] Checking {} years residency for {}...",
            applicant.residency_years, applicant.name);

        let min_years = 2;
        if applicant.residency_years < min_years {
            return Outcome::Fault(format!(
                "Residency rule: {} has {} year(s) residency (minimum: {})",
                applicant.name, applicant.residency_years, min_years
            ));
        }

        applicant.rules_passed.push("residency".to_string());
        println!("  [Rule: Residency] ✓ Passed ({} years ≥ {})",
            applicant.residency_years, min_years);
        Outcome::Next(applicant)
    }
}

// ============================================================================
// Rule 4: Benefit Cap (max 3 concurrent benefits)
// ============================================================================

#[derive(Clone)]
struct BenefitCapRule;

#[async_trait]
impl Transition<Applicant, EligibilityResult> for BenefitCapRule {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        mut applicant: Applicant,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<EligibilityResult, Self::Error> {
        println!("  [Rule: BenefitCap] Checking current benefits ({}) for {}...",
            applicant.current_benefits.len(), applicant.name);

        let max_benefits = 3;
        if applicant.current_benefits.len() >= max_benefits {
            return Outcome::Fault(format!(
                "Benefit cap: {} already has {} benefits (max: {})",
                applicant.name, applicant.current_benefits.len(), max_benefits
            ));
        }

        applicant.rules_passed.push("benefit_cap".to_string());
        println!("  [Rule: BenefitCap] ✓ Passed ({} < {})",
            applicant.current_benefits.len(), max_benefits);

        Outcome::Next(EligibilityResult {
            applicant_id: applicant.id,
            eligible: true,
            rules_passed: applicant.rules_passed,
            approved_benefit: Some("General Assistance".to_string()),
            rejection_reason: None,
        })
    }
}

// ============================================================================
// Main — Demonstrate Rule Chain Pattern
// ============================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Eligibility Rule Chain Pattern ===");
    println!("Pattern: Sequential independent rules with fail-fast rejection");
    println!("Domain example: Welfare benefit eligibility\n");

    let eligibility = Axon::<Applicant, Applicant, String>::new("EligibilityCheck")
        .then(AgeRule)
        .then(IncomeRule)
        .then(ResidencyRule)
        .then(BenefitCapRule);

    if eligibility.maybe_export_and_exit()? {
        return Ok(());
    }

    let applicants = vec![
        ("All rules pass", Applicant {
            id: "WF-001".to_string(),
            name: "Alice Park".to_string(),
            age: 34,
            annual_income: 28_000.0,
            residency_years: 5,
            current_benefits: vec!["Housing".to_string()],
            rules_passed: vec![],
        }),
        ("Income too high", Applicant {
            id: "WF-002".to_string(),
            name: "Bob Lee".to_string(),
            age: 42,
            annual_income: 75_000.0, // exceeds $50k
            residency_years: 10,
            current_benefits: vec![],
            rules_passed: vec![],
        }),
        ("Too young", Applicant {
            id: "WF-003".to_string(),
            name: "Carol Kim".to_string(),
            age: 16, // under 18
            annual_income: 0.0,
            residency_years: 16,
            current_benefits: vec![],
            rules_passed: vec![],
        }),
        ("Benefit cap reached", Applicant {
            id: "WF-004".to_string(),
            name: "David Cho".to_string(),
            age: 50,
            annual_income: 32_000.0,
            residency_years: 20,
            current_benefits: vec![
                "Housing".to_string(),
                "Medical".to_string(),
                "Food".to_string(),
            ], // already at cap
            rules_passed: vec![],
        }),
        ("Insufficient residency", Applicant {
            id: "WF-005".to_string(),
            name: "Eve Yoon".to_string(),
            age: 29,
            annual_income: 22_000.0,
            residency_years: 1, // less than 2
            current_benefits: vec![],
            rules_passed: vec![],
        }),
    ];

    for (i, (label, applicant)) in applicants.into_iter().enumerate() {
        println!("--- Scenario {}: {} ---\n", i + 1, label);
        let mut bus = Bus::new();

        match eligibility.execute(applicant, &(), &mut bus).await {
            Outcome::Next(result) => {
                println!("\n  ELIGIBLE: {} passed {} rules: {:?}",
                    result.applicant_id, result.rules_passed.len(), result.rules_passed);
                if let Some(benefit) = &result.approved_benefit {
                    println!("  Approved benefit: {}", benefit);
                }
            }
            Outcome::Fault(err) => {
                println!("\n  REJECTED: {}", err);
            }
            other => println!("\n  Unexpected: {:?}", other),
        }
        println!();
    }

    // ── Summary ──────────────────────────────────────────────────
    println!("=== Rule Chain Summary ===");
    println!("  1. Each transition = one independent, testable rule");
    println!("  2. Rules can be reordered without code changes");
    println!("  3. Outcome::Fault = rejection (fail-fast, no further rules)");
    println!("  4. Outcome::Next = passed, proceed to next rule");
    println!("  5. Same pattern works for: loan approval, feature flags, access control");

    Ok(())
}
