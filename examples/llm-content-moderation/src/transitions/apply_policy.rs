use ranvier_core::prelude::*;
use ranvier_macros::transition;
use crate::models::{ModerationCategory, ModerationResult, PolicyAction};

/// Apply business-rule moderation policy based on the LLM classification.
///
/// Policy matrix:
/// - Safe (any confidence)                  -> Approve
/// - Unsafe category + confidence >= 0.9    -> Reject
/// - Unsafe category + confidence 0.7..0.9  -> Flag for manual review
/// - Unsafe category + confidence < 0.7     -> Approve (low-confidence, allow through)
#[transition]
pub async fn apply_policy(
    input: serde_json::Value,
    _res: &(),
    _bus: &mut Bus,
) -> Outcome<serde_json::Value, String> {
    let text = input["text"].as_str().unwrap_or("").to_string();
    let user_id = input["user_id"].as_str().unwrap_or("").to_string();

    let moderation: ModerationResult = match serde_json::from_value(input["moderation"].clone()) {
        Ok(m) => m,
        Err(e) => return Outcome::Fault(format!("Failed to parse moderation result: {e}")),
    };

    let action = decide_action(&moderation);

    tracing::info!(
        user_id = %user_id,
        category = ?moderation.category,
        confidence = moderation.confidence,
        action = ?action,
        "Policy decision rendered"
    );

    let response = serde_json::json!({
        "text": text,
        "user_id": user_id,
        "moderation": {
            "category": moderation.category,
            "confidence": moderation.confidence,
            "reasoning": moderation.reasoning,
        },
        "action": action,
    });

    Outcome::Next(response)
}

/// Pure decision function: map (category, confidence) to a PolicyAction.
fn decide_action(result: &ModerationResult) -> PolicyAction {
    // Safe content is always approved regardless of confidence
    if result.category == ModerationCategory::Safe {
        return PolicyAction::Approve;
    }

    let category_label = format!("{:?}", result.category);

    if result.confidence >= 0.9 {
        PolicyAction::Reject {
            reason: format!(
                "Content classified as {category_label} with {:.0}% confidence: {}",
                result.confidence * 100.0,
                result.reasoning,
            ),
        }
    } else if result.confidence >= 0.7 {
        PolicyAction::Flag {
            reason: format!(
                "Content may be {category_label} ({:.0}% confidence) — flagged for manual review: {}",
                result.confidence * 100.0,
                result.reasoning,
            ),
        }
    } else {
        // Low confidence — let it through but log
        PolicyAction::Approve
    }
}
