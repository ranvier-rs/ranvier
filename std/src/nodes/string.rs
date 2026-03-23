use async_trait::async_trait;
use ranvier_core::{bus::Bus, outcome::Outcome, transition::Transition};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub enum StringOperation {
    Append(String),
    Prepend(String),
    ToUpper,
    ToLower,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StringNode {
    pub operation: StringOperation,
}

impl StringNode {
    pub fn new(operation: StringOperation) -> Self {
        Self { operation }
    }
}

#[async_trait]
impl Transition<String, String> for StringNode {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: String,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        match &self.operation {
            StringOperation::Append(s) => Outcome::next(format!("{}{}", input, s)),
            StringOperation::Prepend(s) => Outcome::next(format!("{}{}", s, input)),
            StringOperation::ToUpper => Outcome::next(input.to_uppercase()),
            StringOperation::ToLower => Outcome::next(input.to_lowercase()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn string_append() {
        let node = StringNode::new(StringOperation::Append(" world".into()));
        let mut bus = Bus::new();
        let result = node.run("hello".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(ref v) if v == "hello world"));
    }

    #[tokio::test]
    async fn string_prepend() {
        let node = StringNode::new(StringOperation::Prepend("prefix_".into()));
        let mut bus = Bus::new();
        let result = node.run("data".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(ref v) if v == "prefix_data"));
    }

    #[tokio::test]
    async fn string_to_upper() {
        let node = StringNode::new(StringOperation::ToUpper);
        let mut bus = Bus::new();
        let result = node.run("hello".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(ref v) if v == "HELLO"));
    }

    #[tokio::test]
    async fn string_to_lower() {
        let node = StringNode::new(StringOperation::ToLower);
        let mut bus = Bus::new();
        let result = node.run("HELLO".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(ref v) if v == "hello"));
    }
}
