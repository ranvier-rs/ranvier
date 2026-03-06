use super::*;

#[tokio::test]
async fn audit_event_logging_basic() {
    let sink = InMemoryAuditSink::new();
    let logger = AuditLogger::new(sink.clone());

    let event = AuditEvent::new(
        "evt_001".into(),
        "user_55".into(),
        "DELETE".into(),
        "resource_99".into(),
    )
    .with_intent("Account cleanup")
    .with_metadata("reason", "GDPR request");

    logger.log(event).await.unwrap();

    let recorded = sink.get_events().await;
    assert_eq!(recorded.len(), 1);

    let stored = &recorded[0];
    assert_eq!(stored.id, "evt_001");
    assert_eq!(stored.actor, "user_55");
    assert_eq!(stored.intent.as_deref(), Some("Account cleanup"));
    assert!(stored.metadata.contains_key("reason"));
}

#[tokio::test]
async fn audit_chain_links_events_with_hashes() {
    let chain = AuditChain::new();

    let e1 = AuditEvent::new("1".into(), "admin".into(), "CREATE".into(), "user".into());
    let e2 = AuditEvent::new("2".into(), "admin".into(), "UPDATE".into(), "user".into());

    let linked1 = chain.append(e1).await;
    let linked2 = chain.append(e2).await;

    assert!(linked1.prev_hash.is_none(), "First event has no prev_hash");
    assert!(
        linked2.prev_hash.is_some(),
        "Second event links to first"
    );
    assert_eq!(linked2.prev_hash.unwrap(), linked1.compute_hash());
}

#[tokio::test]
async fn audit_chain_verify_intact_chain() {
    let chain = AuditChain::new();

    for i in 0..5 {
        let event = AuditEvent::new(
            format!("evt_{i}"),
            "system".into(),
            "LOG".into(),
            "service".into(),
        );
        chain.append(event).await;
    }

    assert!(chain.verify().await.is_ok(), "Intact chain should verify");
    assert_eq!(chain.len().await, 5);
}

#[tokio::test]
async fn audit_chain_detect_tampered_event() {
    let chain = AuditChain::new();

    let e1 = AuditEvent::new("1".into(), "admin".into(), "CREATE".into(), "user".into());
    let e2 = AuditEvent::new("2".into(), "admin".into(), "UPDATE".into(), "user".into());

    chain.append(e1).await;
    chain.append(e2).await;

    // Tamper: modify an event in the chain
    {
        let mut events = chain.events.lock().await;
        events[0].actor = "attacker".into();
    }

    let result = chain.verify().await;
    assert!(result.is_err(), "Tampered chain should fail verification");
    if let Err(AuditError::IntegrityViolation { index, .. }) = result {
        assert_eq!(index, 1, "Violation detected at event following the tampered one");
    }
}

#[tokio::test]
async fn audit_chain_detect_deleted_event() {
    let chain = AuditChain::new();

    for i in 0..3 {
        let event = AuditEvent::new(
            format!("evt_{i}"),
            "system".into(),
            "LOG".into(),
            "svc".into(),
        );
        chain.append(event).await;
    }

    // Remove the middle event
    {
        let mut events = chain.events.lock().await;
        events.remove(1);
    }

    let result = chain.verify().await;
    assert!(result.is_err(), "Chain with deleted event should fail");
}

#[tokio::test]
async fn query_filter_by_action() {
    let sink = InMemoryAuditSink::new();

    sink.append(&AuditEvent::new("1".into(), "u1".into(), "CREATE".into(), "t".into()))
        .await
        .unwrap();
    sink.append(&AuditEvent::new("2".into(), "u1".into(), "DELETE".into(), "t".into()))
        .await
        .unwrap();
    sink.append(&AuditEvent::new("3".into(), "u2".into(), "CREATE".into(), "t".into()))
        .await
        .unwrap();

    let query = AuditQuery::new().action("CREATE");
    let results = sink.query(&query).await.unwrap();
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|e| e.action == "CREATE"));
}

#[tokio::test]
async fn query_filter_by_actor() {
    let sink = InMemoryAuditSink::new();

    sink.append(&AuditEvent::new("1".into(), "alice".into(), "READ".into(), "doc".into()))
        .await
        .unwrap();
    sink.append(&AuditEvent::new("2".into(), "bob".into(), "READ".into(), "doc".into()))
        .await
        .unwrap();
    sink.append(&AuditEvent::new("3".into(), "alice".into(), "WRITE".into(), "doc".into()))
        .await
        .unwrap();

    let query = AuditQuery::new().actor("alice");
    let results = sink.query(&query).await.unwrap();
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|e| e.actor == "alice"));
}

#[tokio::test]
async fn query_filter_by_time_range() {
    let sink = InMemoryAuditSink::new();
    let now = Utc::now();

    let mut old = AuditEvent::new("old".into(), "u".into(), "LOG".into(), "t".into());
    old.timestamp = now - Duration::hours(2);

    let mut recent = AuditEvent::new("recent".into(), "u".into(), "LOG".into(), "t".into());
    recent.timestamp = now - Duration::minutes(30);

    let mut fresh = AuditEvent::new("fresh".into(), "u".into(), "LOG".into(), "t".into());
    fresh.timestamp = now;

    sink.append(&old).await.unwrap();
    sink.append(&recent).await.unwrap();
    sink.append(&fresh).await.unwrap();

    let query = AuditQuery::new().time_range(now - Duration::hours(1), now);
    let results = sink.query(&query).await.unwrap();
    assert_eq!(results.len(), 2);
    assert!(results.iter().any(|e| e.id == "recent"));
    assert!(results.iter().any(|e| e.id == "fresh"));
}

#[tokio::test]
async fn query_combined_filters() {
    let sink = InMemoryAuditSink::new();

    sink.append(&AuditEvent::new("1".into(), "alice".into(), "CREATE".into(), "user".into()))
        .await
        .unwrap();
    sink.append(&AuditEvent::new("2".into(), "alice".into(), "DELETE".into(), "user".into()))
        .await
        .unwrap();
    sink.append(&AuditEvent::new("3".into(), "bob".into(), "CREATE".into(), "user".into()))
        .await
        .unwrap();

    let query = AuditQuery::new().actor("alice").action("CREATE");
    let results = sink.query(&query).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "1");
}

#[tokio::test]
async fn retention_max_count() {
    let sink = InMemoryAuditSink::new();

    for i in 0..10 {
        sink.append(&AuditEvent::new(
            format!("evt_{i}"),
            "sys".into(),
            "LOG".into(),
            "svc".into(),
        ))
        .await
        .unwrap();
    }

    let policy = RetentionPolicy::max_count(5);
    let expired = sink.apply_retention(&policy).await.unwrap();

    assert_eq!(expired.len(), 5, "5 oldest events should be expired");
    assert_eq!(sink.len().await, 5, "5 newest events should remain");

    // Verify the remaining events are the newest
    let events = sink.get_events().await;
    assert_eq!(events[0].id, "evt_5");
    assert_eq!(events[4].id, "evt_9");
}

#[tokio::test]
async fn retention_max_age() {
    let sink = InMemoryAuditSink::new();
    let now = Utc::now();

    for i in 0..5 {
        let mut event = AuditEvent::new(
            format!("evt_{i}"),
            "sys".into(),
            "LOG".into(),
            "svc".into(),
        );
        // Events 0-2 are 2 hours old, events 3-4 are fresh
        event.timestamp = if i < 3 {
            now - Duration::hours(2)
        } else {
            now
        };
        sink.append(&event).await.unwrap();
    }

    let policy = RetentionPolicy::max_age(Duration::hours(1));
    let expired = sink.apply_retention(&policy).await.unwrap();

    assert_eq!(expired.len(), 3, "3 old events should be expired");
    assert_eq!(sink.len().await, 2, "2 fresh events should remain");
}

#[tokio::test]
async fn retention_archive_strategy() {
    let policy = RetentionPolicy::max_count(3).with_strategy(ArchiveStrategy::Archive);
    assert_eq!(policy.strategy, ArchiveStrategy::Archive);
}

#[tokio::test]
async fn event_hash_deterministic() {
    let event = AuditEvent::new("id".into(), "actor".into(), "action".into(), "target".into());
    let hash1 = event.compute_hash();
    let hash2 = event.compute_hash();
    assert_eq!(hash1, hash2, "Hash should be deterministic");
    assert_eq!(hash1.len(), 64, "SHA-256 hex should be 64 chars");
}

#[tokio::test]
async fn in_memory_sink_implements_query() {
    let sink = InMemoryAuditSink::new();

    sink.append(&AuditEvent::new("1".into(), "u".into(), "A".into(), "t".into()))
        .await
        .unwrap();
    sink.append(&AuditEvent::new("2".into(), "u".into(), "B".into(), "t".into()))
        .await
        .unwrap();

    let query = AuditQuery::new().target("t");
    let results = sink.query(&query).await.unwrap();
    assert_eq!(results.len(), 2);
}
