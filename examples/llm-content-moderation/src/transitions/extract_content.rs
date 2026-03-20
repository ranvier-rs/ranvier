use ranvier_core::prelude::*;
use ranvier_macros::transition;
use crate::models::ContentInput;

/// Extract and validate user content received via `post_typed()`.
///
/// No manual JSON parsing needed: the HTTP ingress auto-deserializes
/// the request body into `ContentInput` and passes it as the pipeline input.
#[transition]
pub async fn extract_content(
    content: ContentInput,
    _res: &(),
    _bus: &mut Bus,
) -> Outcome<serde_json::Value, String> {
    if content.text.trim().is_empty() {
        return Outcome::Fault("Content text cannot be empty".to_string());
    }

    if content.user_id.trim().is_empty() {
        return Outcome::Fault("user_id cannot be empty".to_string());
    }

    tracing::info!(
        user_id = %content.user_id,
        text_len = content.text.len(),
        "Content extracted for moderation"
    );

    Outcome::Next(serde_json::to_value(&content).unwrap())
}
