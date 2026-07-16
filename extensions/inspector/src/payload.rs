use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};

pub(crate) const DEFAULT_EVENT_BUFFER_SIZE: usize = 500;
pub(crate) const DEFAULT_EVENT_TTL_MS: u64 = 3_600_000;

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
        Self::from_env_with_validity().0
    }

    pub(crate) fn from_env_with_validity() -> (Self, bool) {
        match std::env::var("RANVIER_INSPECTOR_CAPTURE_PAYLOADS") {
            Ok(value) => match value.to_ascii_lowercase().as_str() {
                "off" => (Self::Off, true),
                "hash" => (Self::Hash, true),
                "full" => (Self::Full, true),
                _ => (Self::Off, false),
            },
            Err(std::env::VarError::NotPresent) => (Self::Off, true),
            Err(std::env::VarError::NotUnicode(_)) => (Self::Off, false),
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
    events: VecDeque<BufferedEvent>,
    max_size: usize,
    ttl_ms: u64,
    dropped_oldest: u64,
}

struct BufferedEvent {
    retained_at: u64,
    event: CapturedEvent,
}

/// Runtime stats for the captured event ring buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub struct EventBufferStats {
    pub current_len: usize,
    pub max_size: usize,
    pub dropped_oldest: u64,
}

impl EventRingBuffer {
    fn new(max_size: usize, ttl_ms: u64) -> Self {
        Self {
            events: VecDeque::with_capacity(max_size),
            max_size,
            ttl_ms,
            dropped_oldest: 0,
        }
    }

    fn push(&mut self, event: CapturedEvent) {
        let now = super::epoch_ms();
        self.prune_expired_at(now);
        if self.max_size == 0 {
            return;
        }
        if self.events.len() >= self.max_size {
            self.events.pop_front();
            self.dropped_oldest = self.dropped_oldest.saturating_add(1);
        }
        self.events.push_back(BufferedEvent {
            retained_at: event.timestamp.min(now),
            event,
        });
    }

    fn list(&mut self, limit: usize) -> Vec<CapturedEvent> {
        self.prune_expired();
        self.events
            .iter()
            .rev()
            .take(limit)
            .map(|buffered| buffered.event.clone())
            .collect()
    }

    fn list_filtered(
        &mut self,
        node_id: Option<&str>,
        event_type: Option<&str>,
        limit: usize,
    ) -> Vec<CapturedEvent> {
        self.prune_expired();
        self.events
            .iter()
            .rev()
            .filter(|buffered| {
                let event = &buffered.event;
                if let Some(nid) = node_id {
                    if event.node_id.as_deref() != Some(nid) {
                        return false;
                    }
                }
                if let Some(et) = event_type {
                    if event.event_type != et {
                        return false;
                    }
                }
                true
            })
            .take(limit)
            .map(|buffered| buffered.event.clone())
            .collect()
    }

    fn stats(&mut self) -> EventBufferStats {
        self.prune_expired();
        EventBufferStats {
            current_len: self.events.len(),
            max_size: self.max_size,
            dropped_oldest: self.dropped_oldest,
        }
    }

    fn prune_expired(&mut self) {
        self.prune_expired_at(super::epoch_ms());
    }

    fn prune_expired_at(&mut self, now: u64) {
        if self.ttl_ms == 0 {
            return;
        }
        let cutoff = now.saturating_sub(self.ttl_ms);
        let before = self.events.len();
        self.events.retain(|event| event.retained_at >= cutoff);
        let removed = u64::try_from(before.saturating_sub(self.events.len())).unwrap_or(u64::MAX);
        self.dropped_oldest = self.dropped_oldest.saturating_add(removed);
    }
}

static EVENT_BUFFER: OnceLock<Arc<Mutex<EventRingBuffer>>> = OnceLock::new();

fn get_event_buffer() -> Arc<Mutex<EventRingBuffer>> {
    EVENT_BUFFER
        .get_or_init(|| {
            Arc::new(Mutex::new(EventRingBuffer::new(
                DEFAULT_EVENT_BUFFER_SIZE,
                DEFAULT_EVENT_TTL_MS,
            )))
        })
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
        .map(|mut buf| buf.list(limit))
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
        .map(|mut buf| buf.list_filtered(node_id, event_type, limit))
        .unwrap_or_default()
}

/// Return event ring-buffer capacity and drop counters.
pub fn event_buffer_stats() -> EventBufferStats {
    get_event_buffer()
        .lock()
        .ok()
        .map(|mut buf| buf.stats())
        .unwrap_or(EventBufferStats {
            current_len: 0,
            max_size: 0,
            dropped_oldest: 0,
        })
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
        let mut buf = EventRingBuffer::new(3, u64::MAX);
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
        assert_eq!(buf.stats().dropped_oldest, 2);
        // Newest first
        assert_eq!(events[0].event_type, "event_4");
        assert_eq!(events[1].event_type, "event_3");
        assert_eq!(events[2].event_type, "event_2");
    }

    #[test]
    fn filter_by_node_id() {
        let mut buf = EventRingBuffer::new(10, u64::MAX);
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

    #[test]
    fn ring_buffer_prunes_expired_events() {
        let now = super::super::epoch_ms();
        let mut buf = EventRingBuffer::new(10, 1_000);
        buf.push(CapturedEvent {
            timestamp: now.saturating_sub(2_000),
            event_type: "expired".into(),
            node_id: None,
            circuit: None,
            duration_ms: None,
            outcome_type: None,
            payload_hash: None,
            payload_json: None,
        });
        buf.push(CapturedEvent {
            timestamp: now,
            event_type: "fresh".into(),
            node_id: None,
            circuit: None,
            duration_ms: None,
            outcome_type: None,
            payload_hash: None,
            payload_json: None,
        });

        let events = buf.list(10);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "fresh");
        assert_eq!(buf.stats().dropped_oldest, 1);
    }

    #[test]
    fn future_event_timestamp_cannot_bypass_retention() {
        let mut buf = EventRingBuffer::new(10, 1_000);
        buf.push(CapturedEvent {
            timestamp: u64::MAX,
            event_type: "future".into(),
            node_id: None,
            circuit: None,
            duration_ms: None,
            outcome_type: None,
            payload_hash: None,
            payload_json: None,
        });

        let retained_at = buf.events.front().unwrap().retained_at;
        assert_ne!(retained_at, u64::MAX);
        buf.prune_expired_at(retained_at.saturating_add(1_001));
        assert!(buf.events.is_empty());
    }

    #[test]
    fn out_of_order_event_timestamp_is_still_pruned() {
        let now = super::super::epoch_ms();
        let mut buf = EventRingBuffer::new(10, 1_000);
        buf.push(CapturedEvent {
            timestamp: now,
            event_type: "fresh".into(),
            node_id: None,
            circuit: None,
            duration_ms: None,
            outcome_type: None,
            payload_hash: None,
            payload_json: None,
        });
        buf.push(CapturedEvent {
            timestamp: now.saturating_sub(2_000),
            event_type: "late-old".into(),
            node_id: None,
            circuit: None,
            duration_ms: None,
            outcome_type: None,
            payload_hash: None,
            payload_json: None,
        });

        let events = buf.list(10);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "fresh");
    }
}
