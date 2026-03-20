use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// User-submitted content to be moderated.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ContentInput {
    pub text: String,
    pub user_id: String,
}

/// Result of the mock-LLM classification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModerationResult {
    pub category: ModerationCategory,
    pub confidence: f64,
    pub reasoning: String,
}

/// Content safety categories produced by the classifier.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ModerationCategory {
    Safe,
    Spam,
    Harassment,
    Hate,
    Violence,
    Adult,
    Other,
}

/// Final policy decision after applying business rules.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PolicyAction {
    Approve,
    Reject { reason: String },
    Flag { reason: String },
}
