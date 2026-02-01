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
    type Error = std::convert::Infallible;

    async fn run(&self, input: String, _bus: &mut Bus) -> Outcome<String, Self::Error> {
        match &self.operation {
            StringOperation::Append(s) => Outcome::next(format!("{}{}", input, s)),
            StringOperation::Prepend(s) => Outcome::next(format!("{}{}", s, input)),
            StringOperation::ToUpper => Outcome::next(input.to_uppercase()),
            StringOperation::ToLower => Outcome::next(input.to_lowercase()),
        }
    }
}
