//! State Tree Demo - Typed State Tree Example
//!
//! This example demonstrates the core Ranvier philosophy:
//! > Code structure IS execution structure
//! > State transitions flow through enum + match decision trees
//!
//! This is a minimal example showing:
//! 1. Defining a state enum (FlowState pattern)
//! 2. Implementing transitions between states
//! 3. Using the Bus for resource injection
//! 4. Executing the state tree

use ranvier_flow::{FlowState, Transition};
use ranvier_runtime::Bus;

// ============================================================================
// 1. Define your Application State as an Enum
// ============================================================================

/// Application state enum - this IS your execution flow graph
#[derive(Debug, Clone)]
enum AppState {
    /// Initial state: raw input received
    RawInput(String),
    /// Validated state: input has been validated
    Validated(ValidatedInput),
    /// Processed state: business logic applied
    Processed(ProcessedResult),
}

#[derive(Debug, Clone)]
struct ValidatedInput {
    value: String,
    is_premium: bool,
}

#[derive(Debug, Clone)]
struct ProcessedResult {
    output: String,
    processing_time_ms: u64,
}

/// Error enum - also part of the state tree
#[derive(Debug, Clone)]
enum AppError {
    ValidationFailed(String),
    ProcessingFailed(String),
}

// ============================================================================
// 2. Define Transitions between States
// ============================================================================

/// Transition: RawInput -> ValidatedInput (with possible branching)
struct ValidationTransition;

impl Transition<String, ValidatedInput> for ValidationTransition {
    type Error = AppError;
    type Context = Bus;

    fn transition(input: String, ctx: &Self::Context) -> Result<ValidatedInput, Self::Error> {
        // Check if input is valid (non-empty)
        if input.is_empty() {
            return Err(AppError::ValidationFailed("Input cannot be empty".into()));
        }

        // Check for premium status from Bus
        let is_premium = ctx.get::<PremiumStatus>().map(|s| s.0).unwrap_or(false);

        Ok(ValidatedInput {
            value: input.to_uppercase(), // Transform as example
            is_premium,
        })
    }
}

/// Transition: ValidatedInput -> ProcessedResult
struct ProcessingTransition;

impl Transition<ValidatedInput, ProcessedResult> for ProcessingTransition {
    type Error = AppError;
    type Context = Bus;

    fn transition(
        input: ValidatedInput,
        _ctx: &Self::Context,
    ) -> Result<ProcessedResult, Self::Error> {
        // Simulate processing
        let output = if input.is_premium {
            format!("⭐ PREMIUM: {}", input.value)
        } else {
            format!("Standard: {}", input.value)
        };

        Ok(ProcessedResult {
            output,
            processing_time_ms: 42, // Simulated
        })
    }
}

// ============================================================================
// 3. Resource types for the Bus
// ============================================================================

#[derive(Debug, Clone)]
struct PremiumStatus(bool);

// ============================================================================
// 4. Main - Execute the State Tree
// ============================================================================

fn main() {
    println!("=== Ranvier State Tree Demo ===\n");

    // Set up the Bus with resources
    let mut bus = Bus::new();
    bus.insert(PremiumStatus(true)); // User is premium

    // Define the initial input
    let input = "hello, ranvier!".to_string();
    println!("Input: {:?}", input);

    // Execute the state tree manually (showing the pattern)
    // In real usage, this would be orchestrated by the Executor

    // Step 1: Validate
    let validated = match ValidationTransition::transition(input, &bus) {
        Ok(v) => {
            println!("✓ Validation passed: {:?}", v);
            v
        }
        Err(e) => {
            println!("✗ Validation failed: {:?}", e);
            return;
        }
    };

    // Step 2: Process
    let processed = match ProcessingTransition::transition(validated, &bus) {
        Ok(p) => {
            println!("✓ Processing complete: {:?}", p);
            p
        }
        Err(e) => {
            println!("✗ Processing failed: {:?}", e);
            return;
        }
    };

    // Final output
    println!("\n=== Final Result ===");
    println!("Output: {}", processed.output);
    println!("Time: {}ms", processed.processing_time_ms);

    // Demonstrate FlowState usage
    println!("\n=== FlowState Demonstration ===");
    let state: FlowState<ProcessedResult, AppError> = FlowState::terminal(processed);
    println!("Is terminal: {}", state.is_terminal());
    println!("Is active: {}", state.is_active());

    println!("\n✅ Demo complete - Typed State Tree pattern demonstrated!");
}
