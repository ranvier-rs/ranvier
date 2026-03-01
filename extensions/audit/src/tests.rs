use super::*;
use std::sync::{Arc, Mutex};

/// A simple mock sink for testing that stores events in memory.
#[derive(Default, Clone)]
pub struct MockAuditSink {
    events: Arc<Mutex<Vec<AuditEvent>>>,
}

impl MockAuditSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_events(&self) -> Vec<AuditEvent> {
        self.events.lock().unwrap().clone()
    }
}

#[async_trait]
impl AuditSink for MockAuditSink {
    async fn append(&self, event: &AuditEvent) -> Result<(), AuditError> {
        self.events.lock().unwrap().push(event.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_audit_event_logging() {
        let sink = MockAuditSink::new();
        let logger = AuditLogger::new(sink.clone());

        let event = AuditEvent::new(
            "evt_001".into(),
            "user_55".into(),
            "DELETE".into(),
            "resource_99".into()
        )
        .with_intent("Account cleanup")
        .with_metadata("reason", "GDPR request");

        logger.log(event).await.unwrap();

        let recorded = sink.get_events();
        assert_eq!(recorded.len(), 1);
        
        let stored = &recorded[0];
        assert_eq!(stored.id, "evt_001");
        assert_eq!(stored.actor, "user_55");
        assert_eq!(stored.intent.as_deref(), Some("Account cleanup"));
        assert!(stored.metadata.contains_key("reason"));
    }
}
