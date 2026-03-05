use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};

/// Controls whether and how payloads are captured by the Inspector.
///
/// Configured via `RANVIER_INSPECTOR_CAPTURE_PAYLOADS` environment variable:
/// - `off` (default): no payload capture, zero overhead
/// - `hash`: capture only `payload_hash` (SHA-256 prefix), no raw data
/// - `full`: capture redacted payload JSON
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PayloadCapturePolicy {
    Off,
    Hash,
    Full,
}

impl PayloadCapturePolicy {
    pub fn from_env() -> Self {
        match std::env::var("RANVIER_INSPECTOR_CAPTURE_PAYLOADS")
            .unwrap_or_default()
            .to_lowercase()
            .as_str()
        {
            "full" => Self::Full,
            "hash" => Self::Hash,
            _ => Self::Off,
        }
    }
}

/// A captured event record for the event stream.
#[derive(Clone, Debug, serde::Serialize)]
pub struct CapturedEvent {
    pub timestamp: u64,
    pub event_type: String,
    pub node_id: Option<String>,
    pub circuit: Option<String>,
    pub duration_ms: Option<u64>,
    pub outcome_type: Option<String>,
    pub payload_hash: Option<String>,
    pub payload_json: Option<serde_json::Value>,
}

/// Ring buffer of recent captured events for the event stream panel.
struct EventRingBuffer {
    events: VecDeque<CapturedEvent>,
    max_size: usize,
}

impl EventRingBuffer {
    fn new(max_size: usize) -> Self {
        Self {
            events: VecDeque::with_capacity(max_size),
            max_size,
        }
    }

    fn push(&mut self, event: CapturedEvent) {
        if self.events.len() >= self.max_size {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    fn list(&self, limit: usize) -> Vec<CapturedEvent> {
        self.events.iter().rev().take(limit).cloned().collect()
    }

    fn list_filtered(
        &self,
        node_id: Option<&str>,
        event_type: Option<&str>,
        limit: usize,
    ) -> Vec<CapturedEvent> {
        self.events
            .iter()
            .rev()
            .filter(|e| {
                if let Some(nid) = node_id {
                    if e.node_id.as_deref() != Some(nid) {
                        return false;
                    }
                }
                if let Some(et) = event_type {
                    if e.event_type != et {
                        return false;
                    }
                }
                true
            })
            .take(limit)
            .cloned()
            .collect()
    }
}

static EVENT_BUFFER: OnceLock<Arc<Mutex<EventRingBuffer>>> = OnceLock::new();

fn get_event_buffer() -> Arc<Mutex<EventRingBuffer>> {
    EVENT_BUFFER
        .get_or_init(|| Arc::new(Mutex::new(EventRingBuffer::new(500))))
        .clone()
}

/// Record a captured event into the ring buffer.
pub fn record_event(event: CapturedEvent) {
    if let Ok(mut buf) = get_event_buffer().lock() {
        buf.push(event);
    }
}

/// List recent events, newest first.
pub fn list_events(limit: usize) -> Vec<CapturedEvent> {
    get_event_buffer()
        .lock()
        .ok()
        .map(|buf| buf.list(limit))
        .unwrap_or_default()
}

/// List recent events with optional filters.
pub fn list_events_filtered(
    node_id: Option<&str>,
    event_type: Option<&str>,
    limit: usize,
) -> Vec<CapturedEvent> {
    get_event_buffer()
        .lock()
        .ok()
        .map(|buf| buf.list_filtered(node_id, event_type, limit))
        .unwrap_or_default()
}

/// Compute a short hash prefix for payload identification without storing data.
pub fn payload_hash(data: &[u8]) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    data.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_defaults_to_off() {
        // When env var is not set, should default to Off
        let policy = PayloadCapturePolicy::Off;
        assert_eq!(policy, PayloadCapturePolicy::Off);
    }

    #[test]
    fn ring_buffer_evicts_oldest() {
        let mut buf = EventRingBuffer::new(3);
        for i in 0..5 {
            buf.push(CapturedEvent {
                timestamp: i,
                event_type: format!("event_{i}"),
                node_id: None,
                circuit: None,
                duration_ms: None,
                outcome_type: None,
                payload_hash: None,
                payload_json: None,
            });
        }
        let events = buf.list(10);
        assert_eq!(events.len(), 3);
        // Newest first
        assert_eq!(events[0].event_type, "event_4");
        assert_eq!(events[1].event_type, "event_3");
        assert_eq!(events[2].event_type, "event_2");
    }

    #[test]
    fn filter_by_node_id() {
        let mut buf = EventRingBuffer::new(10);
        buf.push(CapturedEvent {
            timestamp: 1,
            event_type: "node_exit".into(),
            node_id: Some("a".into()),
            circuit: None,
            duration_ms: None,
            outcome_type: None,
            payload_hash: None,
            payload_json: None,
        });
        buf.push(CapturedEvent {
            timestamp: 2,
            event_type: "node_exit".into(),
            node_id: Some("b".into()),
            circuit: None,
            duration_ms: None,
            outcome_type: None,
            payload_hash: None,
            payload_json: None,
        });
        let filtered = buf.list_filtered(Some("a"), None, 10);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].node_id.as_deref(), Some("a"));
    }

    #[test]
    fn payload_hash_is_deterministic() {
        let h1 = payload_hash(b"hello world");
        let h2 = payload_hash(b"hello world");
        assert_eq!(h1, h2);
        let h3 = payload_hash(b"different");
        assert_ne!(h1, h3);
    }
}
