//! # LLM-as-Transition: Model LLM API calls as Ranvier Transitions
//!
//! This module provides [`LlmTransition`], which wraps an LLM API call into
//! the Ranvier decision-engine pipeline. Prompt templates reference Bus data
//! via `{{variable}}` (string) and `{{json:variable}}` (JSON-serialized) syntax.
//!
//! ## Example
//!
//! ```rust,ignore
//! use ranvier_runtime::llm::{LlmTransition, LlmProvider};
//!
//! let llm = LlmTransition::new(LlmProvider::Claude)
//!     .model("claude-sonnet-4-5-20250929")
//!     .system_prompt("You are a content moderator.")
//!     .prompt_template("Classify the following content: {{content}}")
//!     .max_tokens(200)
//!     .temperature(0.3)
//!     .retry_count(2);
//! ```
//!
//! ## Mock Provider
//!
//! By default (no feature flags), only [`LlmProvider::Mock`] actually executes.
//! The mock provider returns a configurable response, making it ideal for CI and
//! unit tests. Real providers (`Claude`, `OpenAI`) are behind feature gates
//! `llm-claude` and `llm-openai` respectively.

use async_trait::async_trait;
use ranvier_core::bus::Bus;
use ranvier_core::outcome::Outcome;
use ranvier_core::transition::Transition;
use serde::{Deserialize, Serialize};
use std::fmt;

// ---------------------------------------------------------------------------
// LlmProvider
// ---------------------------------------------------------------------------

/// Which LLM backend to use.
///
/// Concrete HTTP calls are behind feature gates; the default build only
/// includes [`Mock`](LlmProvider::Mock).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum LlmProvider {
    /// A deterministic mock provider for testing and CI.
    Mock,
    /// Anthropic Claude API (requires feature `llm-claude`).
    Claude,
    /// OpenAI API (requires feature `llm-openai`).
    OpenAI,
    /// An arbitrary provider identified by name.
    Custom(String),
}

impl fmt::Display for LlmProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LlmProvider::Mock => write!(f, "mock"),
            LlmProvider::Claude => write!(f, "claude"),
            LlmProvider::OpenAI => write!(f, "openai"),
            LlmProvider::Custom(name) => write!(f, "custom:{name}"),
        }
    }
}

// ---------------------------------------------------------------------------
// LlmError
// ---------------------------------------------------------------------------

/// Errors produced by an LLM transition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LlmError {
    /// The selected provider is not available (feature not enabled or
    /// unsupported [`LlmProvider::Custom`] variant).
    ProviderUnavailable { provider: String, reason: String },
    /// The prompt template references a variable that was not found on the Bus.
    TemplateMissing { variable: String },
    /// The LLM call failed after exhausting all retries.
    RequestFailed {
        provider: String,
        attempts: u32,
        last_error: String,
    },
    /// The LLM returned a response that does not match the expected output
    /// schema.
    SchemaValidation {
        expected_schema: serde_json::Value,
        raw_response: String,
        reason: String,
    },
    /// The response could not be parsed as JSON.
    ResponseParse {
        raw_response: String,
        reason: String,
    },
}

impl fmt::Display for LlmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LlmError::ProviderUnavailable { provider, reason } => {
                write!(f, "LLM provider `{provider}` unavailable: {reason}")
            }
            LlmError::TemplateMissing { variable } => {
                write!(f, "template variable `{variable}` not found on Bus")
            }
            LlmError::RequestFailed {
                provider,
                attempts,
                last_error,
            } => {
                write!(
                    f,
                    "LLM request to `{provider}` failed after {attempts} attempt(s): {last_error}"
                )
            }
            LlmError::SchemaValidation {
                reason,
                raw_response,
                ..
            } => {
                write!(
                    f,
                    "LLM response schema validation failed: {reason} (response: {raw_response})"
                )
            }
            LlmError::ResponseParse {
                raw_response,
                reason,
            } => {
                write!(
                    f,
                    "failed to parse LLM response as JSON: {reason} (response: {raw_response})"
                )
            }
        }
    }
}

impl std::error::Error for LlmError {}

// ---------------------------------------------------------------------------
// MockLlmConfig — Bus resource for configuring mock responses
// ---------------------------------------------------------------------------

/// Configuration for the [`LlmProvider::Mock`] provider.
///
/// Insert this into the [`Bus`] before executing an Axon that contains an
/// `LlmTransition` with `LlmProvider::Mock` to control its output.
///
/// ```rust,ignore
/// bus.provide(MockLlmConfig {
///     response: r#"{"label":"safe","confidence":0.99}"#.to_string(),
///     ..Default::default()
/// });
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MockLlmConfig {
    /// The raw text response the mock provider should return.
    pub response: String,
    /// If `true`, the mock provider simulates a request failure.
    pub should_fail: bool,
    /// Error message used when `should_fail` is `true`.
    pub failure_message: String,
}

impl Default for MockLlmConfig {
    fn default() -> Self {
        Self {
            response: r#"{"result":"mock_response"}"#.to_string(),
            should_fail: false,
            failure_message: "simulated mock failure".to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// LlmTransition
// ---------------------------------------------------------------------------

/// Wraps an LLM API call as a Ranvier [`Transition`].
///
/// ## Builder API
///
/// ```rust,ignore
/// LlmTransition::new(LlmProvider::Claude)
///     .model("claude-sonnet-4-5-20250929")
///     .system_prompt("You are a helpful assistant.")
///     .prompt_template("Classify: {{content}}")
///     .output_schema::<ModerationResult>()
///     .max_tokens(200)
///     .temperature(0.3)
///     .retry_count(2)
///     .with_label("ContentModeration")
/// ```
///
/// ## Template Syntax
///
/// - `{{variable}}` — substituted with the `String` value of `variable` from
///   the Bus (via `bus.read::<LlmTemplateVars>()`).
/// - `{{json:variable}}` — substituted with the JSON-serialized representation
///   of `variable` from the Bus template vars.
///
/// ## Transition Contract
///
/// - **Input** (`String`): an optional runtime prompt override; when empty the
///   stored `prompt_template` is used.
/// - **Output** (`String`): the raw LLM response text.
/// - **Error** ([`LlmError`]): typed fault for template, network, or
///   validation failures.
#[derive(Clone)]
pub struct LlmTransition {
    provider: LlmProvider,
    model: Option<String>,
    system_prompt: Option<String>,
    prompt_template: Option<String>,
    max_tokens: Option<u32>,
    temperature: Option<f32>,
    retry_count: u32,
    output_schema: Option<serde_json::Value>,
    label_override: Option<String>,
}

impl fmt::Debug for LlmTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LlmTransition")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("max_tokens", &self.max_tokens)
            .field("temperature", &self.temperature)
            .field("retry_count", &self.retry_count)
            .field("has_output_schema", &self.output_schema.is_some())
            .finish()
    }
}

impl LlmTransition {
    // ----- constructor -----------------------------------------------------

    /// Create a new `LlmTransition` targeting `provider`.
    pub fn new(provider: LlmProvider) -> Self {
        Self {
            provider,
            model: None,
            system_prompt: None,
            prompt_template: None,
            max_tokens: None,
            temperature: None,
            retry_count: 0,
            output_schema: None,
            label_override: None,
        }
    }

    // ----- builder methods -------------------------------------------------

    /// Set the model identifier (e.g. `"claude-sonnet-4-5-20250929"`, `"gpt-4o"`).
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Set an optional system prompt prepended to the conversation.
    pub fn system_prompt(mut self, system: impl Into<String>) -> Self {
        self.system_prompt = Some(system.into());
        self
    }

    /// Set the prompt template.
    ///
    /// Template variables use `{{variable}}` for string substitution and
    /// `{{json:variable}}` for JSON-serialized substitution from the Bus.
    pub fn prompt_template(mut self, template: impl Into<String>) -> Self {
        self.prompt_template = Some(template.into());
        self
    }

    /// Set the maximum number of tokens to generate.
    pub fn max_tokens(mut self, max: u32) -> Self {
        self.max_tokens = Some(max);
        self
    }

    /// Set the sampling temperature (0.0 = deterministic, 1.0+ = creative).
    pub fn temperature(mut self, temp: f32) -> Self {
        self.temperature = Some(temp);
        self
    }

    /// Set the number of automatic retries on transient failure (default 0).
    pub fn retry_count(mut self, count: u32) -> Self {
        self.retry_count = count;
        self
    }

    /// Override the transition label shown in the Schematic.
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label_override = Some(label.into());
        self
    }

    /// Store a JSON Schema derived from `T` for response validation.
    ///
    /// When set, the LLM response will be parsed as JSON and validated against
    /// this schema. If validation fails the transition returns
    /// [`Outcome::Fault`] with [`LlmError::SchemaValidation`].
    ///
    /// The schema is generated from the `serde_json` representation of `T`.
    /// For richer schema generation, enable the `schema` feature and use
    /// [`schemars`](https://docs.rs/schemars).
    pub fn output_schema<T: Serialize + for<'de> Deserialize<'de> + Default>(mut self) -> Self {
        // Generate a minimal schema by serializing the Default value of T.
        // This gives us the field names and types for basic validation.
        // For full JSON Schema support, use the `schema` feature with schemars.
        let sample = T::default();
        if let Ok(value) = serde_json::to_value(&sample) {
            self.output_schema = Some(infer_schema_from_value(&value));
        }
        self
    }

    /// Set a raw JSON Schema value directly for response validation.
    pub fn output_schema_raw(mut self, schema: serde_json::Value) -> Self {
        self.output_schema = Some(schema);
        self
    }

    // ----- template rendering ----------------------------------------------

    /// Render the prompt template by substituting variables from the Bus.
    ///
    /// Variables are sourced from [`LlmTemplateVars`] on the Bus.
    fn render_prompt(&self, template: &str, bus: &Bus) -> Result<String, LlmError> {
        let vars = bus.read::<LlmTemplateVars>();
        let mut result = template.to_string();

        // Collect all `{{json:var}}` references first (longer pattern).
        let json_re = "{{json:";
        while let Some(start) = result.find(json_re) {
            let after = start + json_re.len();
            let end = result[after..]
                .find("}}")
                .map(|i| after + i)
                .ok_or_else(|| LlmError::TemplateMissing {
                    variable: result[after..].to_string(),
                })?;
            let var_name = &result[after..end];
            let value =
                vars.and_then(|v| v.get(var_name))
                    .ok_or_else(|| LlmError::TemplateMissing {
                        variable: var_name.to_string(),
                    })?;
            let json_str = serde_json::to_string(value).unwrap_or_default();
            result.replace_range(start..end + 2, &json_str);
        }

        // Then `{{var}}` references.
        let simple_re = "{{";
        while let Some(start) = result.find(simple_re) {
            let after = start + simple_re.len();
            let end = result[after..]
                .find("}}")
                .map(|i| after + i)
                .ok_or_else(|| LlmError::TemplateMissing {
                    variable: result[after..].to_string(),
                })?;
            let var_name = &result[after..end];
            let value =
                vars.and_then(|v| v.get(var_name))
                    .ok_or_else(|| LlmError::TemplateMissing {
                        variable: var_name.to_string(),
                    })?;
            let plain_str = match value {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            result.replace_range(start..end + 2, &plain_str);
        }

        Ok(result)
    }

    // ----- validation ------------------------------------------------------

    /// Validate the raw LLM response against the stored output schema.
    fn validate_response(&self, raw: &str) -> Result<(), LlmError> {
        let Some(schema) = &self.output_schema else {
            return Ok(());
        };

        let parsed: serde_json::Value =
            serde_json::from_str(raw).map_err(|e| LlmError::ResponseParse {
                raw_response: raw.to_string(),
                reason: e.to_string(),
            })?;

        validate_value_against_schema(&parsed, schema).map_err(|reason| {
            LlmError::SchemaValidation {
                expected_schema: schema.clone(),
                raw_response: raw.to_string(),
                reason,
            }
        })
    }

    // ----- provider dispatch -----------------------------------------------

    /// Execute the LLM call for the configured provider.
    async fn call_provider(&self, prompt: &str) -> Result<String, String> {
        match &self.provider {
            LlmProvider::Mock => self.call_mock(prompt),
            LlmProvider::Claude => {
                Err("Claude provider requires feature `llm-claude` (not yet implemented)".into())
            }
            LlmProvider::OpenAI => {
                Err("OpenAI provider requires feature `llm-openai` (not yet implemented)".into())
            }
            LlmProvider::Custom(name) => Err(format!(
                "Custom provider `{name}` has no built-in implementation; \
                 use a custom Transition instead"
            )),
        }
    }

    /// Mock provider implementation — reads [`MockLlmConfig`] from a captured ref.
    fn call_mock(&self, _prompt: &str) -> Result<String, String> {
        // The mock config is checked during `run` before calling this method.
        // This path is only reached when no config is on the Bus, in which
        // case we return the default mock response.
        Ok(MockLlmConfig::default().response)
    }

    /// Mock provider implementation with Bus access.
    fn call_mock_with_config(
        &self,
        _prompt: &str,
        config: &MockLlmConfig,
    ) -> Result<String, String> {
        if config.should_fail {
            Err(config.failure_message.clone())
        } else {
            Ok(config.response.clone())
        }
    }
}

// ---------------------------------------------------------------------------
// LlmTemplateVars — Bus resource holding template variables
// ---------------------------------------------------------------------------

/// Key-value map inserted into the [`Bus`] to supply template variables to
/// [`LlmTransition`].
///
/// ```rust,ignore
/// let mut vars = LlmTemplateVars::new();
/// vars.set("content", serde_json::json!("Some user content"));
/// bus.provide(vars);
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LlmTemplateVars {
    inner: serde_json::Map<String, serde_json::Value>,
}

impl LlmTemplateVars {
    /// Create an empty variable map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a template variable.
    pub fn set(&mut self, key: impl Into<String>, value: serde_json::Value) -> &mut Self {
        self.inner.insert(key.into(), value);
        self
    }

    /// Get a template variable by name.
    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.inner.get(key)
    }

    /// Returns `true` if the map contains the given key.
    pub fn contains(&self, key: &str) -> bool {
        self.inner.contains_key(key)
    }

    /// Iterate over all variables.
    pub fn iter(&self) -> serde_json::map::Iter<'_> {
        self.inner.iter()
    }
}

// ---------------------------------------------------------------------------
// Transition implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl Transition<String, String> for LlmTransition {
    type Error = LlmError;
    type Resources = ();

    fn label(&self) -> String {
        self.label_override
            .clone()
            .unwrap_or_else(|| format!("LLM:{}", self.provider))
    }

    fn description(&self) -> Option<String> {
        let model = self.model.as_deref().unwrap_or("default");
        Some(format!(
            "LLM call via {} (model={model}, max_tokens={}, temp={})",
            self.provider,
            self.max_tokens.unwrap_or(0),
            self.temperature.unwrap_or(1.0),
        ))
    }

    async fn run(
        &self,
        input: String,
        _resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        // 1. Determine the prompt: if input is non-empty use it as override,
        //    otherwise render the stored template.
        let prompt = if input.is_empty() {
            match &self.prompt_template {
                Some(tpl) => match self.render_prompt(tpl, bus) {
                    Ok(rendered) => rendered,
                    Err(e) => return Outcome::Fault(e),
                },
                None => {
                    return Outcome::Fault(LlmError::TemplateMissing {
                        variable: "(no prompt template or input provided)".into(),
                    });
                }
            }
        } else if self.prompt_template.is_some() {
            // Input provided AND template exists: substitute the input into the
            // template as if it were a variable called "input".
            let tpl = self
                .prompt_template
                .as_ref()
                .expect("prompt_template guaranteed by is_some() guard");
            let with_input = tpl.replace("{{input}}", &input);
            match self.render_prompt(&with_input, bus) {
                Ok(rendered) => rendered,
                Err(e) => return Outcome::Fault(e),
            }
        } else {
            input
        };

        // 2. Build formatted prompt with system prompt if present.
        let full_prompt = match &self.system_prompt {
            Some(sys) => format!("[system]\n{sys}\n\n[user]\n{prompt}"),
            None => prompt,
        };

        tracing::debug!(
            provider = %self.provider,
            model = ?self.model,
            prompt_len = full_prompt.len(),
            "LlmTransition executing"
        );

        // 3. Execute with retries.
        let max_attempts = self.retry_count + 1;
        let mut last_error = String::new();

        for attempt in 1..=max_attempts {
            let result = match &self.provider {
                LlmProvider::Mock => {
                    // Read mock config from Bus each attempt (allows test mutation).
                    match bus.read::<MockLlmConfig>() {
                        Some(cfg) => self.call_mock_with_config(&full_prompt, cfg),
                        None => self.call_mock(&full_prompt),
                    }
                }
                _ => self.call_provider(&full_prompt).await,
            };

            match result {
                Ok(response) => {
                    // 4. Validate against output schema if present.
                    if let Err(e) = self.validate_response(&response) {
                        tracing::warn!(
                            attempt,
                            provider = %self.provider,
                            "LLM response failed schema validation"
                        );
                        // Schema validation failures are not retried — the LLM
                        // returned *something*, it just doesn't match.
                        return Outcome::Fault(e);
                    }

                    tracing::debug!(
                        attempt,
                        provider = %self.provider,
                        response_len = response.len(),
                        "LlmTransition completed"
                    );
                    return Outcome::Next(response);
                }
                Err(err) => {
                    tracing::warn!(
                        attempt,
                        max_attempts,
                        provider = %self.provider,
                        error = %err,
                        "LLM call failed"
                    );
                    last_error = err;
                }
            }
        }

        Outcome::Fault(LlmError::RequestFailed {
            provider: self.provider.to_string(),
            attempts: max_attempts,
            last_error,
        })
    }
}

// ---------------------------------------------------------------------------
// Schema helpers (minimal, no extra deps)
// ---------------------------------------------------------------------------

/// Infer a basic JSON Schema from a sample [`serde_json::Value`].
///
/// This is intentionally simple — it produces `{"type":"object","properties":{...}}`
/// from the keys of a sample value. For full JSON Schema support, users should
/// supply a hand-written schema via [`LlmTransition::output_schema_raw`].
fn infer_schema_from_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut properties = serde_json::Map::new();
            for (key, val) in map {
                properties.insert(key.clone(), infer_schema_from_value(val));
            }
            serde_json::json!({
                "type": "object",
                "properties": properties
            })
        }
        serde_json::Value::Array(arr) => {
            let items = arr
                .first()
                .map(infer_schema_from_value)
                .unwrap_or_else(|| serde_json::json!({}));
            serde_json::json!({
                "type": "array",
                "items": items
            })
        }
        serde_json::Value::String(_) => serde_json::json!({"type": "string"}),
        serde_json::Value::Number(_) => serde_json::json!({"type": "number"}),
        serde_json::Value::Bool(_) => serde_json::json!({"type": "boolean"}),
        serde_json::Value::Null => serde_json::json!({"type": "null"}),
    }
}

/// Validate a parsed JSON value against a minimal schema.
///
/// Checks top-level `type` and, for objects, that all `properties` keys exist
/// and have the expected type.
fn validate_value_against_schema(
    value: &serde_json::Value,
    schema: &serde_json::Value,
) -> Result<(), String> {
    let Some(expected_type) = schema.get("type").and_then(|t| t.as_str()) else {
        // No type constraint — accept anything.
        return Ok(());
    };

    let actual_type = json_type_name(value);
    if actual_type != expected_type {
        return Err(format!(
            "expected type `{expected_type}`, got `{actual_type}`"
        ));
    }

    // For objects, check properties.
    if expected_type == "object" {
        if let (Some(props), Some(obj)) = (
            schema.get("properties").and_then(|p| p.as_object()),
            value.as_object(),
        ) {
            for (key, prop_schema) in props {
                match obj.get(key) {
                    Some(val) => validate_value_against_schema(val, prop_schema)
                        .map_err(|e| format!("property `{key}`: {e}"))?,
                    None => {
                        // Missing key is acceptable unless "required" is set.
                        // Our minimal schema does not enforce required.
                    }
                }
            }
        }
    }

    // For arrays, check items schema against each element.
    if expected_type == "array" {
        if let (Some(items_schema), Some(arr)) = (schema.get("items"), value.as_array()) {
            for (i, elem) in arr.iter().enumerate() {
                validate_value_against_schema(elem, items_schema)
                    .map_err(|e| format!("item[{i}]: {e}"))?;
            }
        }
    }

    Ok(())
}

fn json_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_sets_all_fields() {
        let t = LlmTransition::new(LlmProvider::Claude)
            .model("claude-sonnet-4-5-20250929")
            .system_prompt("You are a moderator.")
            .prompt_template("Classify: {{content}}")
            .max_tokens(200)
            .temperature(0.3)
            .retry_count(2)
            .with_label("ModerationLLM");

        assert_eq!(t.provider, LlmProvider::Claude);
        assert_eq!(t.model.as_deref(), Some("claude-sonnet-4-5-20250929"));
        assert_eq!(t.system_prompt.as_deref(), Some("You are a moderator."));
        assert_eq!(t.prompt_template.as_deref(), Some("Classify: {{content}}"));
        assert_eq!(t.max_tokens, Some(200));
        assert_eq!(t.temperature, Some(0.3));
        assert_eq!(t.retry_count, 2);
        assert_eq!(t.label(), "ModerationLLM");
    }

    #[test]
    fn default_label_includes_provider() {
        let t = LlmTransition::new(LlmProvider::OpenAI);
        assert_eq!(t.label(), "LLM:openai");
    }

    #[test]
    fn template_rendering_simple() {
        let t = LlmTransition::new(LlmProvider::Mock).prompt_template("Hello, {{name}}!");
        let mut bus = Bus::new();
        let mut vars = LlmTemplateVars::new();
        vars.set("name", serde_json::json!("Alice"));
        bus.provide(vars);

        let rendered = t.render_prompt("Hello, {{name}}!", &bus).unwrap();
        assert_eq!(rendered, "Hello, Alice!");
    }

    #[test]
    fn template_rendering_json_var() {
        let t = LlmTransition::new(LlmProvider::Mock);
        let mut bus = Bus::new();
        let mut vars = LlmTemplateVars::new();
        vars.set("data", serde_json::json!({"key": "value"}));
        bus.provide(vars);

        let rendered = t.render_prompt("Payload: {{json:data}}", &bus).unwrap();
        assert_eq!(rendered, r#"Payload: {"key":"value"}"#);
    }

    #[test]
    fn template_missing_variable_returns_error() {
        let t = LlmTransition::new(LlmProvider::Mock);
        let bus = Bus::new();

        let err = t.render_prompt("Hello, {{missing}}!", &bus).unwrap_err();
        assert!(matches!(err, LlmError::TemplateMissing { variable } if variable == "missing"));
    }

    #[tokio::test]
    async fn mock_provider_returns_default_response() {
        let t = LlmTransition::new(LlmProvider::Mock).prompt_template("test prompt");
        let mut bus = Bus::new();
        let mut vars = LlmTemplateVars::new();
        vars.set("_placeholder", serde_json::json!(true));
        bus.provide(vars);

        let outcome = t.run(String::new(), &(), &mut bus).await;
        match outcome {
            Outcome::Next(response) => {
                assert!(response.contains("mock_response"));
            }
            other => panic!("expected Outcome::Next, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mock_provider_with_custom_config() {
        let t = LlmTransition::new(LlmProvider::Mock);
        let mut bus = Bus::new();
        bus.provide(MockLlmConfig {
            response: r#"{"label":"safe"}"#.to_string(),
            ..Default::default()
        });

        let outcome = t.run("direct prompt".to_string(), &(), &mut bus).await;
        match outcome {
            Outcome::Next(response) => {
                assert_eq!(response, r#"{"label":"safe"}"#);
            }
            other => panic!("expected Outcome::Next, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mock_provider_failure_returns_fault() {
        let t = LlmTransition::new(LlmProvider::Mock).retry_count(1);
        let mut bus = Bus::new();
        bus.provide(MockLlmConfig {
            response: String::new(),
            should_fail: true,
            failure_message: "service unavailable".to_string(),
        });

        let outcome = t.run("test".to_string(), &(), &mut bus).await;
        match outcome {
            Outcome::Fault(LlmError::RequestFailed {
                attempts,
                last_error,
                ..
            }) => {
                assert_eq!(attempts, 2); // 1 retry + 1 initial
                assert_eq!(last_error, "service unavailable");
            }
            other => panic!("expected Outcome::Fault(RequestFailed), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn schema_validation_rejects_wrong_type() {
        let t = LlmTransition::new(LlmProvider::Mock).output_schema_raw(serde_json::json!({
            "type": "object",
            "properties": {
                "label": {"type": "string"}
            }
        }));
        let mut bus = Bus::new();
        // Mock returns a string, not an object.
        bus.provide(MockLlmConfig {
            response: r#""just a string""#.to_string(),
            ..Default::default()
        });

        let outcome = t.run("test".to_string(), &(), &mut bus).await;
        assert!(matches!(
            outcome,
            Outcome::Fault(LlmError::SchemaValidation { .. })
        ));
    }

    #[tokio::test]
    async fn schema_validation_accepts_valid_response() {
        let t = LlmTransition::new(LlmProvider::Mock).output_schema_raw(serde_json::json!({
            "type": "object",
            "properties": {
                "label": {"type": "string"},
                "confidence": {"type": "number"}
            }
        }));
        let mut bus = Bus::new();
        bus.provide(MockLlmConfig {
            response: r#"{"label":"safe","confidence":0.95}"#.to_string(),
            ..Default::default()
        });

        let outcome = t.run("test".to_string(), &(), &mut bus).await;
        assert!(matches!(outcome, Outcome::Next(_)));
    }

    #[test]
    fn infer_schema_from_sample_object() {
        let sample = serde_json::json!({"name": "test", "count": 0});
        let schema = infer_schema_from_value(&sample);
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["name"]["type"], "string");
        assert_eq!(schema["properties"]["count"]["type"], "number");
    }

    #[test]
    fn provider_display() {
        assert_eq!(LlmProvider::Mock.to_string(), "mock");
        assert_eq!(LlmProvider::Claude.to_string(), "claude");
        assert_eq!(LlmProvider::OpenAI.to_string(), "openai");
        assert_eq!(
            LlmProvider::Custom("ollama".into()).to_string(),
            "custom:ollama"
        );
    }

    #[test]
    fn llm_error_display_coverage() {
        let err = LlmError::ProviderUnavailable {
            provider: "claude".into(),
            reason: "feature not enabled".into(),
        };
        assert!(err.to_string().contains("claude"));

        let err = LlmError::TemplateMissing {
            variable: "foo".into(),
        };
        assert!(err.to_string().contains("foo"));

        let err = LlmError::RequestFailed {
            provider: "openai".into(),
            attempts: 3,
            last_error: "timeout".into(),
        };
        assert!(err.to_string().contains("3 attempt(s)"));

        let err = LlmError::ResponseParse {
            raw_response: "not json".into(),
            reason: "unexpected token".into(),
        };
        assert!(err.to_string().contains("unexpected token"));
    }

    #[test]
    fn template_vars_api() {
        let mut vars = LlmTemplateVars::new();
        vars.set("key1", serde_json::json!("value1"));
        vars.set("key2", serde_json::json!(42));

        assert!(vars.contains("key1"));
        assert!(!vars.contains("key3"));
        assert_eq!(vars.get("key1").unwrap(), &serde_json::json!("value1"));
        assert_eq!(vars.iter().count(), 2);
    }

    #[tokio::test]
    async fn claude_provider_returns_fault_without_feature() {
        let t = LlmTransition::new(LlmProvider::Claude);
        let mut bus = Bus::new();

        let outcome = t.run("test".to_string(), &(), &mut bus).await;
        assert!(matches!(
            outcome,
            Outcome::Fault(LlmError::RequestFailed { .. })
        ));
    }

    #[test]
    fn description_includes_model_and_params() {
        let t = LlmTransition::new(LlmProvider::Claude)
            .model("claude-sonnet-4-5-20250929")
            .max_tokens(200)
            .temperature(0.3);

        let desc = t.description().unwrap();
        assert!(desc.contains("claude"));
        assert!(desc.contains("claude-sonnet-4-5-20250929"));
        assert!(desc.contains("200"));
    }
}
