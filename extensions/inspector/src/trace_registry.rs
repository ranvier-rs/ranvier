use std::collections::{HashMap, VecDeque};

use super::{TraceRecord, TraceRegistryStats, TraceStatus, epoch_ms};

pub(super) struct TraceRegistryStorage {
    active: HashMap<String, TraceRecord>,
    recent: VecDeque<TraceRecord>,
    max_recent: usize,
    trace_ttl_ms: u64,
    ttl_pruned: u64,
    capacity_evicted: u64,
}

impl TraceRegistryStorage {
    pub(super) fn new(max_recent: usize, trace_ttl_ms: u64) -> Self {
        Self {
            active: HashMap::new(),
            recent: VecDeque::new(),
            max_recent,
            trace_ttl_ms,
            ttl_pruned: 0,
            capacity_evicted: 0,
        }
    }

    pub(super) fn register(&mut self, circuit: String) {
        self.prune_expired();
        if self.max_recent == 0 {
            return;
        }
        while self.active.len() >= self.max_recent {
            let oldest = self
                .active
                .iter()
                .min_by_key(|(_, record)| record.started_at)
                .map(|(trace_id, _)| trace_id.clone());
            let Some(oldest) = oldest else {
                break;
            };
            self.active.remove(&oldest);
            self.capacity_evicted = self.capacity_evicted.saturating_add(1);
        }
        let trace_id = format!(
            "{}-{}",
            circuit.replace(' ', "_").to_lowercase(),
            uuid::Uuid::new_v4()
        );
        self.active.insert(
            trace_id.clone(),
            TraceRecord {
                trace_id,
                circuit,
                status: TraceStatus::Active,
                started_at: epoch_ms(),
                finished_at: None,
                duration_ms: None,
                outcome_type: None,
            },
        );
    }

    pub(super) fn complete(
        &mut self,
        circuit: &str,
        outcome_type: Option<String>,
        duration_ms: Option<u64>,
    ) {
        self.prune_expired();
        let key = self
            .active
            .iter()
            .filter(|(_, record)| record.circuit == circuit)
            .max_by_key(|(_, record)| record.started_at)
            .map(|(key, _)| key.clone());

        if let Some(key) = key {
            if let Some(mut record) = self.active.remove(&key) {
                record.finished_at = Some(epoch_ms());
                record.duration_ms = duration_ms;
                record.outcome_type = outcome_type.clone();
                record.status = if outcome_type.as_deref() == Some("Fault") {
                    TraceStatus::Faulted
                } else {
                    TraceStatus::Completed
                };

                self.recent.push_back(record);
                while self.recent.len() > self.max_recent {
                    self.recent.pop_front();
                    self.capacity_evicted = self.capacity_evicted.saturating_add(1);
                }
            }
        }
    }

    fn prune_expired(&mut self) {
        if self.trace_ttl_ms == 0 {
            return;
        }
        let cutoff = epoch_ms().saturating_sub(self.trace_ttl_ms);
        let active_before = self.active.len();
        self.active.retain(|_, record| record.started_at >= cutoff);
        let pruned_active =
            u64::try_from(active_before.saturating_sub(self.active.len())).unwrap_or(u64::MAX);
        self.ttl_pruned = self.ttl_pruned.saturating_add(pruned_active);
        let recent_before = self.recent.len();
        self.recent.retain(|record| record.started_at >= cutoff);
        let pruned_recent =
            u64::try_from(recent_before.saturating_sub(self.recent.len())).unwrap_or(u64::MAX);
        self.ttl_pruned = self.ttl_pruned.saturating_add(pruned_recent);
    }

    pub(super) fn list_all(&self) -> Vec<TraceRecord> {
        let mut result: Vec<TraceRecord> = self.active.values().cloned().collect();
        result.extend(self.recent.iter().cloned());
        result.sort_by_key(|record| std::cmp::Reverse(record.started_at));
        result
    }

    pub(super) fn active_count(&self) -> usize {
        self.active.len()
    }

    pub(super) fn recent_count(&self) -> usize {
        self.recent.len()
    }

    pub(super) fn stats(&self) -> TraceRegistryStats {
        TraceRegistryStats {
            active_count: self.active.len(),
            recent_count: self.recent.len(),
            max_recent: self.max_recent,
            ttl_pruned: self.ttl_pruned,
            capacity_evicted: self.capacity_evicted,
        }
    }

    pub(super) fn has_config(&self, max_recent: usize, trace_ttl_ms: u64) -> bool {
        self.max_recent == max_recent && self.trace_ttl_ms == trace_ttl_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_evicts_oldest() {
        let mut registry = TraceRegistryStorage::new(3, 0);

        for i in 0..5 {
            let record = TraceRecord {
                trace_id: format!("t-{i}"),
                circuit: "Test".to_string(),
                status: TraceStatus::Completed,
                started_at: 1000 + i * 100,
                finished_at: Some(1100 + i * 100),
                duration_ms: Some(100),
                outcome_type: Some("Next".to_string()),
            };
            registry.recent.push_back(record);
            while registry.recent.len() > registry.max_recent {
                registry.recent.pop_front();
                registry.capacity_evicted = registry.capacity_evicted.saturating_add(1);
            }
        }

        assert_eq!(registry.recent_count(), 3);
        assert_eq!(registry.recent.front().unwrap().trace_id, "t-2");
        assert_eq!(registry.recent.back().unwrap().trace_id, "t-4");
        let stats = registry.stats();
        assert_eq!(stats.capacity_evicted, 2);
        assert_eq!(stats.ttl_pruned, 0);
    }

    #[test]
    fn active_registry_is_capacity_bounded() {
        let mut registry = TraceRegistryStorage::new(2, 60_000);
        registry.register("one".to_string());
        registry.register("two".to_string());
        registry.register("three".to_string());

        assert_eq!(registry.active_count(), 2);
        assert_eq!(registry.stats().capacity_evicted, 1);
    }

    #[test]
    fn active_registry_prunes_expired_entries() {
        let mut registry = TraceRegistryStorage::new(10, 1_000);
        registry.register("expired".to_string());
        for record in registry.active.values_mut() {
            record.started_at = epoch_ms().saturating_sub(2_000);
        }

        registry.register("fresh".to_string());

        assert_eq!(registry.active_count(), 1);
        assert_eq!(registry.stats().ttl_pruned, 1);
    }

    #[test]
    fn ttl_prunes_expired() {
        let now = epoch_ms();
        let mut registry = TraceRegistryStorage::new(100, 1000);

        registry.recent.push_back(TraceRecord {
            trace_id: "old".to_string(),
            circuit: "Test".to_string(),
            status: TraceStatus::Completed,
            started_at: now.saturating_sub(2000),
            finished_at: Some(now.saturating_sub(1900)),
            duration_ms: Some(100),
            outcome_type: Some("Next".to_string()),
        });
        registry.recent.push_back(TraceRecord {
            trace_id: "fresh".to_string(),
            circuit: "Test".to_string(),
            status: TraceStatus::Completed,
            started_at: now,
            finished_at: Some(now + 100),
            duration_ms: Some(100),
            outcome_type: Some("Next".to_string()),
        });

        registry.prune_expired();
        assert_eq!(registry.recent_count(), 1);
        assert_eq!(registry.recent.front().unwrap().trace_id, "fresh");
        assert_eq!(registry.stats().ttl_pruned, 1);
    }

    #[test]
    fn ttl_prunes_out_of_order_recent_records() {
        let now = epoch_ms();
        let mut registry = TraceRegistryStorage::new(100, 1_000);
        registry.recent.push_back(TraceRecord {
            trace_id: "fresh".to_string(),
            circuit: "Test".to_string(),
            status: TraceStatus::Completed,
            started_at: now,
            finished_at: Some(now),
            duration_ms: Some(0),
            outcome_type: Some("Next".to_string()),
        });
        registry.recent.push_back(TraceRecord {
            trace_id: "late-old".to_string(),
            circuit: "Test".to_string(),
            status: TraceStatus::Completed,
            started_at: now.saturating_sub(2_000),
            finished_at: Some(now),
            duration_ms: Some(2_000),
            outcome_type: Some("Next".to_string()),
        });

        registry.prune_expired();
        assert_eq!(registry.recent_count(), 1);
        assert_eq!(registry.recent.front().unwrap().trace_id, "fresh");
        assert_eq!(registry.stats().ttl_pruned, 1);
    }

    #[test]
    fn config_defaults() {
        let config = super::super::TraceRegistryConfig::default();
        assert_eq!(config.max_traces, 10_000);
        assert_eq!(config.trace_ttl, std::time::Duration::from_secs(3600));
    }

    #[test]
    fn public_wrapper_forwards_registry_configuration() {
        let registry = super::super::ActiveTraceRegistry::with_config(7, 500);
        assert_eq!(registry.stats().max_recent, 7);
        assert_eq!(registry.stats().active_count, 0);
        assert_eq!(registry.stats().recent_count, 0);
    }
}
