/*!
# Sensor Decision Loop Pattern

## Purpose
Demonstrates the **sensor-decision-loop pattern** using Ranvier's Axon pipeline.
Sensor data flows through evaluation stages that produce an actuator decision.

## Pattern: Sensor → Threshold → Decision → Action
Read sensor data, evaluate against thresholds, make a decision, and determine
the appropriate action. Each stage is a composable Transition.

## Applied Domain: Smart Farm
Soil moisture + temperature sensors → threshold evaluation → irrigation decision.

## Key Concepts
- **Outcome::Next**: Data continues through the pipeline
- **Outcome::Branch**: Decision point routes to different actions
- **Bus**: Carries sensor metadata and decision context

## Running
```bash
cargo run -p sensor-decision-loop
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
struct SensorReading {
    sensor_id: String,
    soil_moisture: f64, // 0.0 (dry) to 1.0 (saturated)
    temperature: f64,   // Celsius
    timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EvaluatedReading {
    sensor_id: String,
    soil_moisture: f64,
    temperature: f64,
    moisture_status: MoistureStatus,
    temp_status: TempStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum MoistureStatus {
    Critical, // < 0.2
    Low,      // 0.2 - 0.4
    Normal,   // 0.4 - 0.7
    High,     // > 0.7
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum TempStatus {
    Cold,   // < 10°C
    Normal, // 10-35°C
    Hot,    // > 35°C
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ActuatorCommand {
    sensor_id: String,
    action: Action,
    reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum Action {
    IrrigateHigh,  // Full irrigation
    IrrigateLow,   // Light irrigation
    NoAction,      // Everything normal
    AlertOperator, // Abnormal conditions
}

// ============================================================================
// Stage 1: Threshold Evaluation
// ============================================================================

#[derive(Clone)]
struct EvaluateThresholds;

#[async_trait]
impl Transition<SensorReading, EvaluatedReading> for EvaluateThresholds {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        reading: SensorReading,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<EvaluatedReading, Self::Error> {
        println!(
            "  [EvaluateThresholds] Sensor {}: moisture={:.2}, temp={:.1}°C",
            reading.sensor_id, reading.soil_moisture, reading.temperature
        );

        // Validate sensor data
        if !reading.soil_moisture.is_finite() || !reading.temperature.is_finite() {
            return Outcome::Fault("Sensor readings must be finite".into());
        }
        if reading.soil_moisture < 0.0 || reading.soil_moisture > 1.0 {
            return Outcome::Fault(format!(
                "Invalid moisture reading: {:.2} (expected 0.0-1.0)",
                reading.soil_moisture
            ));
        }

        let moisture_status = match reading.soil_moisture {
            m if m < 0.2 => MoistureStatus::Critical,
            m if m < 0.4 => MoistureStatus::Low,
            m if m <= 0.7 => MoistureStatus::Normal,
            _ => MoistureStatus::High,
        };

        let temp_status = match reading.temperature {
            t if t < 10.0 => TempStatus::Cold,
            t if t <= 35.0 => TempStatus::Normal,
            _ => TempStatus::Hot,
        };

        println!(
            "  [EvaluateThresholds] Moisture: {:?}, Temp: {:?}",
            moisture_status, temp_status
        );

        Outcome::Next(EvaluatedReading {
            sensor_id: reading.sensor_id,
            soil_moisture: reading.soil_moisture,
            temperature: reading.temperature,
            moisture_status,
            temp_status,
        })
    }
}

// ============================================================================
// Stage 2: Decision Engine
// ============================================================================

#[derive(Clone)]
struct MakeDecision;

#[async_trait]
impl Transition<EvaluatedReading, ActuatorCommand> for MakeDecision {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        reading: EvaluatedReading,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<ActuatorCommand, Self::Error> {
        println!(
            "  [MakeDecision] Determining action for sensor {}...",
            reading.sensor_id
        );

        let (action, reason) = match (&reading.moisture_status, &reading.temp_status) {
            // Critical moisture → irrigate regardless
            (MoistureStatus::Critical, _) => (
                Action::IrrigateHigh,
                format!(
                    "Critical moisture ({:.2}) — full irrigation",
                    reading.soil_moisture
                ),
            ),
            // Low moisture + hot → irrigate
            (MoistureStatus::Low, TempStatus::Hot) => (
                Action::IrrigateHigh,
                format!(
                    "Low moisture ({:.2}) + high temp ({:.1}°C) — full irrigation",
                    reading.soil_moisture, reading.temperature
                ),
            ),
            // Low moisture + normal → light irrigation
            (MoistureStatus::Low, TempStatus::Normal) => (
                Action::IrrigateLow,
                format!(
                    "Low moisture ({:.2}) — light irrigation",
                    reading.soil_moisture
                ),
            ),
            // Low moisture + cold → alert (irrigation risk in cold)
            (MoistureStatus::Low, TempStatus::Cold) => (
                Action::AlertOperator,
                format!(
                    "Low moisture ({:.2}) + cold ({:.1}°C) — operator review needed",
                    reading.soil_moisture, reading.temperature
                ),
            ),
            // Normal conditions
            (MoistureStatus::Normal, TempStatus::Normal) => {
                (Action::NoAction, "All readings normal".to_string())
            }
            // Hot but moisture OK → alert
            (MoistureStatus::Normal, TempStatus::Hot) => (
                Action::AlertOperator,
                format!(
                    "High temperature ({:.1}°C) — monitoring recommended",
                    reading.temperature
                ),
            ),
            // High moisture → no irrigation
            (MoistureStatus::High, _) => (
                Action::NoAction,
                format!(
                    "Moisture already high ({:.2}) — no irrigation",
                    reading.soil_moisture
                ),
            ),
            // Cold but moisture OK → no action
            (MoistureStatus::Normal, TempStatus::Cold) => (
                Action::NoAction,
                "Moisture normal, temperature cold — monitoring only".to_string(),
            ),
        };

        println!("  [MakeDecision] Action: {:?} — {}", action, reason);

        Outcome::Next(ActuatorCommand {
            sensor_id: reading.sensor_id,
            action,
            reason,
        })
    }
}

// ============================================================================
// Main — Demonstrate Sensor Decision Pattern
// ============================================================================

// Explicit local-demo policy: neither persisted nor incomplete cleanup results
// may be converted into ordinary domain success or automatic retry permission.
fn local_demo_outcome<T, E>(
    report: ranvier_runtime::ExecutionReport<Outcome<T, E>>,
) -> anyhow::Result<Outcome<T, E>> {
    if report
        .cleanup()
        .is_some_and(|cleanup| !cleanup.is_complete())
    {
        anyhow::bail!("Execution requires reconciliation before retry");
    }
    match report {
        ranvier_runtime::ExecutionReport::Unpersisted { result, .. } => Ok(result),
        _ => anyhow::bail!("Execution requires reconciliation before retry"),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Sensor Decision Loop Pattern ===");
    println!("Pattern: Sensor → Threshold Evaluation → Decision → Action");
    println!("Domain example: Smart farm irrigation control\n");

    let decision_pipeline = Axon::<SensorReading, SensorReading, String>::new("SensorDecision")
        .then(EvaluateThresholds)
        .then(MakeDecision);

    if decision_pipeline.maybe_export_and_exit()? {
        return Ok(());
    }

    let scenarios = vec![
        (
            "Normal conditions",
            SensorReading {
                sensor_id: "FARM-A1".to_string(),
                soil_moisture: 0.55,
                temperature: 24.0,
                timestamp: "2026-03-28T10:00:00Z".to_string(),
            },
        ),
        (
            "Critical drought",
            SensorReading {
                sensor_id: "FARM-B2".to_string(),
                soil_moisture: 0.12,
                temperature: 32.0,
                timestamp: "2026-03-28T14:00:00Z".to_string(),
            },
        ),
        (
            "Low moisture + heat",
            SensorReading {
                sensor_id: "FARM-C3".to_string(),
                soil_moisture: 0.35,
                temperature: 38.0,
                timestamp: "2026-03-28T15:30:00Z".to_string(),
            },
        ),
        (
            "Low moisture + frost risk",
            SensorReading {
                sensor_id: "FARM-D4".to_string(),
                soil_moisture: 0.28,
                temperature: 3.0,
                timestamp: "2026-03-28T05:00:00Z".to_string(),
            },
        ),
        (
            "Invalid sensor data",
            SensorReading {
                sensor_id: "FARM-E5".to_string(),
                soil_moisture: -0.5, // invalid
                temperature: 20.0,
                timestamp: "2026-03-28T12:00:00Z".to_string(),
            },
        ),
    ];

    for (i, (label, reading)) in scenarios.into_iter().enumerate() {
        println!("--- Scenario {}: {} ---\n", i + 1, label);
        let mut bus = Bus::new();

        match local_demo_outcome(decision_pipeline.execute(reading, &(), &mut bus).await)? {
            Outcome::Next(cmd) => {
                println!("\n  Command: {:?}", cmd.action);
                println!("  Reason: {}", cmd.reason);
            }
            Outcome::Fault(err) => {
                println!("\n  Sensor error: {}", err);
            }
            _ => anyhow::bail!("Unexpected domain control flow in local demo"),
        }
        println!();
    }

    // ── Summary ──────────────────────────────────────────────────
    println!("=== Sensor Decision Loop Summary ===");
    println!("  1. SensorReading → EvaluatedReading → ActuatorCommand (typed progression)");
    println!("  2. Threshold evaluation is a separate, testable transition");
    println!("  3. Decision engine combines multiple factors into a single action");
    println!("  4. Outcome::Fault catches invalid sensor data early");
    println!("  5. Each stage is independently deployable and replaceable");

    Ok(())
}

#[cfg(test)]
mod execution_contract_tests {
    use super::*;
    #[tokio::test]
    async fn nonfinite_readings_are_rejected_before_decisions() {
        for (moisture, temperature) in [
            (f64::NAN, 20.0),
            (f64::INFINITY, 20.0),
            (0.5, f64::NAN),
            (0.5, f64::NEG_INFINITY),
        ] {
            let mut bus = Bus::new();
            let input = SensorReading {
                sensor_id: "fixture".into(),
                soil_moisture: moisture,
                temperature,
                timestamp: "fixture".into(),
            };
            assert!(
                matches!(EvaluateThresholds.run(input, &(), &mut bus).await, Outcome::Fault(error) if error == "Sensor readings must be finite")
            );
        }
    }
    use ranvier_runtime::{
        CompletionConfirmation, EffectBoundary, ExecutionReport, PersistenceOperation,
        SagaRollbackOutcome, SagaRollbackReport, SagaRollbackStep,
    };

    #[test]
    fn declared_demo_scenarios_run_through_the_report_policy() {
        main().unwrap();
    }

    #[test]
    fn local_report_policy_preserves_faults_and_rejects_uncertain_or_incomplete_execution() {
        let next = || Outcome::<u32, String>::Next(7);
        let reports = [
            ExecutionReport::NotRequested {
                result: next(),
                cleanup: None,
            },
            ExecutionReport::Recorded {
                result: next(),
                completion: CompletionConfirmation::Confirmed,
                cleanup: None,
            },
            ExecutionReport::Recorded {
                result: next(),
                completion: CompletionConfirmation::NotRequested,
                cleanup: None,
            },
            ExecutionReport::Unconfirmed {
                result: next(),
                operation: PersistenceOperation::Result,
                effect: EffectBoundary::MayHaveRun,
                cleanup: None,
            },
            ExecutionReport::BlockedBeforeEffect {
                operation: PersistenceOperation::Intent,
                effect: EffectBoundary::NotInvoked,
                cleanup: None,
            },
            ExecutionReport::BlockedBeforeEffect {
                operation: PersistenceOperation::Intent,
                effect: EffectBoundary::MayHaveRun,
                cleanup: None,
            },
        ];
        for report in reports {
            assert_eq!(
                local_demo_outcome(report).unwrap_err().to_string(),
                "Execution requires reconciliation before retry"
            );
        }
        for (finished, outcome) in [
            (false, SagaRollbackOutcome::Completed),
            (true, SagaRollbackOutcome::Fault),
            (true, SagaRollbackOutcome::InProgress),
        ] {
            let report = ExecutionReport::Unpersisted {
                result: next(),
                cleanup: Some(SagaRollbackReport {
                    trace_id: "private-trace".into(),
                    finished,
                    steps: vec![SagaRollbackStep {
                        node_id: "private-node".into(),
                        outcome,
                    }],
                }),
            };
            assert_eq!(
                local_demo_outcome(report).unwrap_err().to_string(),
                "Execution requires reconciliation before retry"
            );
        }
        assert!(matches!(
            local_demo_outcome(ExecutionReport::Unpersisted {
                result: next(),
                cleanup: None
            })
            .unwrap(),
            Outcome::Next(7)
        ));
        assert!(
            matches!(local_demo_outcome(ExecutionReport::Unpersisted { result: Outcome::<u32, String>::Fault("expected-domain-failure".into()), cleanup: None }).unwrap(), Outcome::Fault(error) if error == "expected-domain-failure")
        );
    }
}
