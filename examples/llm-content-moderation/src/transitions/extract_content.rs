use ranvier_core::prelude::*;
use ranvier_macros::transition;
use crate::models::ContentInput;

/// Extract user content from the Bus (HTTP body -> ContentInput).
///
/// The HTTP adapter places the raw request body into the Bus as a `String`.
/// This transition parses it into a typed `ContentInput` and forwards it as
/// serialized JSON for the next stage.
#[transition]
pub async fn extract_content(
    _input: (),
    _res: &(),
    bus: &mut Bus,
) -> Outcome<serde_json::Value, String> {
    let body = bus.read::<String>().cloned().unwrap_or_default();

    let content: ContentInput = match serde_json::from_str(&body) {
        Ok(c) => c,
        Err(_) => {
            return Outcome::Fault(
                "Invalid JSON body — expected { \"text\": \"...\", \"user_id\": \"...\" }"
                    .to_string(),
            );
        }
    };

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
