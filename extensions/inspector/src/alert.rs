//! Alert hooks for Inspector production monitoring.
//!
//! Supports webhook-based alerts for stall detection, error rate thresholds,
//! and policy violations. Includes debounce to prevent alert storms.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Alert event types emitted by Inspector monitoring.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AlertEvent {
    /// A circuit step has been running longer than the stall threshold.
    StallDetected {
        circuit: String,
        node_id: String,
        duration_ms: u64,
    },
    /// Circuit error rate exceeded the configured threshold.
    ErrorRate {
        circuit: String,
        rate: f64,
        threshold: f64,
    },
    /// A schematic policy violation was detected.
    PolicyViolation { rule: String, detail: String },
    /// Custom application-defined alert.
    Custom {
        name: String,
        payload: serde_json::Value,
    },
}

impl AlertEvent {
    /// Returns a deduplication key for debounce logic.
    fn dedup_key(&self) -> String {
        match self {
            AlertEvent::StallDetected {
                circuit, node_id, ..
            } => {
                format!("stall:{circuit}:{node_id}")
            }
            AlertEvent::ErrorRate { circuit, .. } => format!("error_rate:{circuit}"),
            AlertEvent::PolicyViolation { rule, .. } => format!("policy:{rule}"),
            AlertEvent::Custom { name, .. } => format!("custom:{name}"),
        }
    }
}

/// Trait for alert delivery backends.
#[async_trait]
pub trait AlertHook: Send + Sync {
    /// Deliver an alert event.
    async fn alert(&self, event: &AlertEvent) -> Result<(), String>;
}

/// Webhook-based alert hook — sends HTTP POST with JSON payload.
pub struct WebhookAlertHook {
    url: String,
    client: reqwest::Client,
    /// Minimum interval between alerts with the same dedup key.
    debounce: Duration,
    last_sent: Mutex<HashMap<String, Instant>>,
}

impl WebhookAlertHook {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            client: reqwest::Client::new(),
            debounce: Duration::from_secs(60),
            last_sent: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_debounce(mut self, debounce: Duration) -> Self {
        self.debounce = debounce;
        self
    }

    fn should_send(&self, key: &str) -> bool {
        let mut map = self.last_sent.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(last) = map.get(key) {
            if last.elapsed() < self.debounce {
                return false;
            }
        }
        map.insert(key.to_string(), Instant::now());
        true
    }
}

#[async_trait]
impl AlertHook for WebhookAlertHook {
    async fn alert(&self, event: &AlertEvent) -> Result<(), String> {
        let key = event.dedup_key();
        if !self.should_send(&key) {
            tracing::debug!(key = %key, "Alert debounced");
            return Ok(());
        }

        let payload = serde_json::json!({
            "source": "ranvier-inspector",
            "event": event,
            "timestamp": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        });

        self.client
            .post(&self.url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| format!("Webhook delivery failed: {e}"))?;

        tracing::info!(url = %self.url, key = %key, "Alert delivered via webhook");
        Ok(())
    }
}

/// Dispatcher that fans out alerts to multiple hooks.
pub struct AlertDispatcher {
    hooks: Vec<Box<dyn AlertHook>>,
}

impl AlertDispatcher {
    pub fn new() -> Self {
        Self { hooks: Vec::new() }
    }

    pub fn add_hook(mut self, hook: impl AlertHook + 'static) -> Self {
        self.hooks.push(Box::new(hook));
        self
    }

    /// Send an alert to all registered hooks. Errors are logged but don't propagate.
    pub async fn dispatch(&self, event: &AlertEvent) {
        for hook in &self.hooks {
            if let Err(e) = hook.alert(event).await {
                tracing::warn!(error = %e, "Alert hook delivery failed");
            }
        }
    }
}

impl Default for AlertDispatcher {
    fn default() -> Self {
        Self::new()
    }
}
