use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StepMetadata {
    pub id: Uuid,
    pub label: String,
    pub description: Option<String>,
    pub inputs: Vec<TypeInfo>,
    pub outputs: Vec<TypeInfo>,
}

impl StepMetadata {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeInfo {
    pub name: String,
    // Additional type metadata can be added here
}

impl TypeInfo {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}
