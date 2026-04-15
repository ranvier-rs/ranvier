use crate::models::{ContentInput, ModerationCategory, ModerationResult};
use ranvier_core::prelude::*;
use ranvier_macros::transition;

/// Mock LLM classification transition.
///
/// In production this would call an LLM API (e.g. OpenAI, Anthropic) to
/// classify content. For CI/demo purposes we simulate classification with
/// keyword pattern matching and return a `ModerationResult`.
#[transition]
pub async fn moderate_content(
    input: serde_json::Value,
    _res: &(),
    _bus: &mut Bus,
) -> Outcome<serde_json::Value, String> {
    let content: ContentInput = match serde_json::from_value(input) {
        Ok(c) => c,
        Err(e) => return Outcome::Fault(format!("Failed to parse content input: {e}")),
    };

    tracing::info!(
        user_id = %content.user_id,
        "Invoking mock LLM classifier (simulated latency: 120ms, tokens: ~45)"
    );

    // Simulate LLM latency
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;

    let text_lower = content.text.to_lowercase();
    let result = classify_text(&text_lower);

    tracing::info!(
        user_id = %content.user_id,
        category = ?result.category,
        confidence = result.confidence,
        "Mock LLM classification complete"
    );

    // Combine original content with moderation result for the next stage
    let output = serde_json::json!({
        "text": content.text,
        "user_id": content.user_id,
        "moderation": result,
    });

    Outcome::Next(output)
}

/// Simulate LLM classification via keyword matching.
///
/// Returns a `ModerationResult` with a category and confidence score.
fn classify_text(text: &str) -> ModerationResult {
    // Spam indicators
    let spam_keywords = [
        "buy now",
        "free money",
        "click here",
        "spam",
        "limited offer",
    ];
    if let Some(keyword) = spam_keywords.iter().find(|kw| text.contains(*kw)) {
        return ModerationResult {
            category: ModerationCategory::Spam,
            confidence: 0.95,
            reasoning: format!("Detected spam indicator: \"{keyword}\""),
        };
    }

    // Harassment indicators
    let harassment_keywords = ["harass", "bully", "threaten", "stalk"];
    if let Some(keyword) = harassment_keywords.iter().find(|kw| text.contains(*kw)) {
        return ModerationResult {
            category: ModerationCategory::Harassment,
            confidence: 0.88,
            reasoning: format!("Detected harassment indicator: \"{keyword}\""),
        };
    }

    // Hate speech indicators
    let hate_keywords = ["hate", "slur", "bigot", "discriminat"];
    if let Some(keyword) = hate_keywords.iter().find(|kw| text.contains(*kw)) {
        return ModerationResult {
            category: ModerationCategory::Hate,
            confidence: 0.92,
            reasoning: format!("Detected hate speech indicator: \"{keyword}\""),
        };
    }

    // Violence indicators
    let violence_keywords = ["kill", "attack", "violence", "weapon", "bomb"];
    if let Some(keyword) = violence_keywords.iter().find(|kw| text.contains(*kw)) {
        return ModerationResult {
            category: ModerationCategory::Violence,
            confidence: 0.85,
            reasoning: format!("Detected violence indicator: \"{keyword}\""),
        };
    }

    // Adult content indicators
    let adult_keywords = ["explicit", "nsfw", "adult content"];
    if let Some(keyword) = adult_keywords.iter().find(|kw| text.contains(*kw)) {
        return ModerationResult {
            category: ModerationCategory::Adult,
            confidence: 0.90,
            reasoning: format!("Detected adult content indicator: \"{keyword}\""),
        };
    }

    // Default: content is safe
    ModerationResult {
        category: ModerationCategory::Safe,
        confidence: 0.97,
        reasoning: "No policy-violating content detected".to_string(),
    }
}
