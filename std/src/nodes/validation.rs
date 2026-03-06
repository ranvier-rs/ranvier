//! Validation nodes for input checking within Ranvier circuits.

use async_trait::async_trait;
use ranvier_core::{bus::Bus, outcome::Outcome, transition::Transition};
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::marker::PhantomData;

/// Validates that an `Option<T>` is `Some`, faulting if `None`.
#[derive(Debug, Clone)]
pub struct RequiredNode<T> {
    field_name: String,
    _marker: PhantomData<T>,
}

impl<T> RequiredNode<T> {
    pub fn new(field_name: impl Into<String>) -> Self {
        Self {
            field_name: field_name.into(),
            _marker: PhantomData,
        }
    }
}

#[async_trait]
impl<T> Transition<Option<T>, T> for RequiredNode<T>
where
    T: Send + Sync + 'static,
{
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: Option<T>,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<T, Self::Error> {
        match input {
            Some(value) => Outcome::next(value),
            None => Outcome::fault(format!("Required field '{}' is missing", self.field_name)),
        }
    }
}

/// Validates that a numeric value falls within a specified range.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RangeValidator<T> {
    pub min: T,
    pub max: T,
    pub field_name: String,
}

impl<T> RangeValidator<T> {
    pub fn new(min: T, max: T, field_name: impl Into<String>) -> Self {
        Self {
            min,
            max,
            field_name: field_name.into(),
        }
    }
}

#[async_trait]
impl<T> Transition<T, T> for RangeValidator<T>
where
    T: PartialOrd + Debug + Send + Sync + Clone + 'static,
{
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: T,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<T, Self::Error> {
        if input < self.min || input > self.max {
            Outcome::fault(format!(
                "Field '{}' value {:?} out of range [{:?}, {:?}]",
                self.field_name, input, self.min, self.max
            ))
        } else {
            Outcome::next(input)
        }
    }
}

/// Validates that a string matches a regex pattern.
#[derive(Debug, Clone)]
pub struct PatternValidator {
    pattern: String,
    field_name: String,
}

impl PatternValidator {
    pub fn new(pattern: impl Into<String>, field_name: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            field_name: field_name.into(),
        }
    }
}

#[async_trait]
impl Transition<String, String> for PatternValidator {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: String,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        // Simple glob-like pattern matching (contains check)
        // For full regex, users would use the `regex` crate directly
        if input.contains(&self.pattern) || self.pattern == "*" {
            Outcome::next(input)
        } else {
            Outcome::fault(format!(
                "Field '{}' value '{}' does not match pattern '{}'",
                self.field_name, input, self.pattern
            ))
        }
    }
}

/// Validates a JSON value against expected structure.
#[derive(Debug, Clone)]
pub struct SchemaValidator {
    required_fields: Vec<String>,
}

impl SchemaValidator {
    pub fn new(required_fields: Vec<String>) -> Self {
        Self { required_fields }
    }
}

#[async_trait]
impl Transition<serde_json::Value, serde_json::Value> for SchemaValidator {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: serde_json::Value,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<serde_json::Value, Self::Error> {
        if let serde_json::Value::Object(ref map) = input {
            for field in &self.required_fields {
                if !map.contains_key(field) {
                    return Outcome::fault(format!("Missing required field: '{field}'"));
                }
            }
            Outcome::next(input)
        } else {
            Outcome::fault("Expected JSON object".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn required_node_passes_some() {
        let node = RequiredNode::<i32>::new("count");
        let mut bus = Bus::new();
        let result = node.run(Some(42), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(42)));
    }

    #[tokio::test]
    async fn required_node_faults_none() {
        let node = RequiredNode::<i32>::new("count");
        let mut bus = Bus::new();
        let result = node.run(None, &(), &mut bus).await;
        assert!(matches!(result, Outcome::Fault(_)));
    }

    #[tokio::test]
    async fn range_validator_in_range() {
        let node = RangeValidator::new(1, 100, "age");
        let mut bus = Bus::new();
        let result = node.run(25, &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(25)));
    }

    #[tokio::test]
    async fn range_validator_out_of_range() {
        let node = RangeValidator::new(1, 100, "age");
        let mut bus = Bus::new();
        let result = node.run(200, &(), &mut bus).await;
        assert!(matches!(result, Outcome::Fault(_)));
    }

    #[tokio::test]
    async fn pattern_validator_matches() {
        let node = PatternValidator::new("@", "email");
        let mut bus = Bus::new();
        let result = node.run("user@example.com".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
    }

    #[tokio::test]
    async fn pattern_validator_no_match() {
        let node = PatternValidator::new("@", "email");
        let mut bus = Bus::new();
        let result = node.run("invalid-email".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Fault(_)));
    }

    #[tokio::test]
    async fn schema_validator_passes() {
        let node = SchemaValidator::new(vec!["name".into(), "age".into()]);
        let mut bus = Bus::new();
        let input = serde_json::json!({"name": "Alice", "age": 30});
        let result = node.run(input, &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
    }

    #[tokio::test]
    async fn schema_validator_missing_field() {
        let node = SchemaValidator::new(vec!["name".into(), "age".into()]);
        let mut bus = Bus::new();
        let input = serde_json::json!({"name": "Alice"});
        let result = node.run(input, &(), &mut bus).await;
        assert!(matches!(result, Outcome::Fault(_)));
    }
}
