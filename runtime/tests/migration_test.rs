use ranvier_core::prelude::*;
use ranvier_core::schematic::{
    MigrationRegistry, MigrationStrategy, SchemaMigrationMapper, SnapshotMigration,
};
use ranvier_runtime::Axon;
use ranvier_runtime::persistence::{InMemoryPersistenceStore, PersistenceHandle};
use ranvier_runtime::replay::replay_and_recover;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum TestError {
    Fail,
}

#[derive(Clone)]
struct StepCounter {
    pub count: Arc<Mutex<u32>>,
}

#[async_trait::async_trait]
impl Transition<u32, u32> for StepCounter {
    type Error = TestError;
    type Resources = ();

    async fn run(
        &self,
        state: u32,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<u32, Self::Error> {
        let mut guard = self.count.lock().await;
        *guard += 1;
        Outcome::next(state + 1)
    }
}

#[tokio::test]
async fn test_migration_resume_from_start() {
    let counter = StepCounter {
        count: Arc::new(Mutex::new(0)),
    };
    let store = InMemoryPersistenceStore::new();
    let handle = PersistenceHandle::from_store(store.clone());

    // Schematic v1.0
    let axon_v10 = Axon::<u32, u32, TestError, ()>::new("MigrateTest")
        .with_version("v1.0")
        .then(counter.clone()) // Node index 1 (after ingress)
        .then(counter.clone()); // Node index 2

    // Run v1.0 but mock completion/interruption after first step
    // Actually, let's just run it and see it persists.
    let mut bus = Bus::new();
    bus.insert(handle.clone());
    bus.insert(ranvier_runtime::persistence::PersistenceTraceId::new(
        "trace-1",
    ));

    let _ = axon_v10.execute(0, &(), &mut bus).await;

    // Check counter
    {
        let count = *counter.count.lock().await;
        assert_eq!(count, 2);
    }

    // Now define v1.1
    let counter_v11 = StepCounter {
        count: Arc::new(Mutex::new(0)),
    }; // Reset counter for new run
    let axon_v11 = Axon::<u32, u32, TestError, ()>::new("MigrateTest")
        .with_version("v1.1")
        .then(counter_v11.clone())
        .then(counter_v11.clone())
        .then(counter_v11.clone());

    // Register migration v1.0 -> v1.1: ResumeFromStart
    let mut registry = MigrationRegistry::new("MigrateTest");
    registry.register(SnapshotMigration {
        name: Some("Upgrade to v1.1".to_string()),
        from_version: "v1.0".to_string(),
        to_version: "v1.1".to_string(),
        default_strategy: MigrationStrategy::ResumeFromStart,
        node_mapping: std::collections::HashMap::new(),
        payload_mapper: None,
    });

    let mut bus_v11 = Bus::new();
    bus_v11.insert(handle.clone());
    bus_v11.insert(ranvier_runtime::persistence::PersistenceTraceId::new(
        "trace-1",
    ));
    bus_v11.insert(registry);

    // This should restart from step 0 because of ResumeFromStart
    let _ = axon_v11.execute(0, &(), &mut bus_v11).await;

    // Counter should be 3 (all steps of v1.1)
    {
        let count = *counter_v11.count.lock().await;
        assert_eq!(count, 3);
    }
}

#[tokio::test]
async fn test_migration_fail_by_default() {
    let store = InMemoryPersistenceStore::new();
    let handle = PersistenceHandle::from_store(store.clone());

    let axon_v10 = Axon::<u32, u32, TestError, ()>::new("FailTest")
        .with_version("v1.0")
        .then(StepCounter {
            count: Arc::new(Mutex::new(0)),
        });

    let mut bus = Bus::new();
    bus.insert(handle.clone());
    bus.insert(ranvier_runtime::persistence::PersistenceTraceId::new(
        "trace-fail",
    ));
    let _ = axon_v10.execute(0, &(), &mut bus).await;

    let axon_v11 = Axon::<u32, u32, TestError, ()>::new("FailTest")
        .with_version("v1.1")
        .then(StepCounter {
            count: Arc::new(Mutex::new(0)),
        });

    let mut bus_v11 = Bus::new();
    bus_v11.insert(handle.clone());
    bus_v11.insert(ranvier_runtime::persistence::PersistenceTraceId::new(
        "trace-fail",
    ));
    // No migration registered

    let outcome = axon_v11.execute(0, &(), &mut bus_v11).await;

    // Should be Outcome::Emit with version_mismatch_failed (based on axon.rs:870)
    match outcome {
        Outcome::Emit(event, _) => {
            assert_eq!(event, "execution.resumption.version_mismatch_failed")
        }
        _ => panic!("expected version mismatch failure, got {:?}", outcome),
    }
}

#[tokio::test]
async fn test_migration_migrate_active_node() {
    let counter_v10 = StepCounter {
        count: Arc::new(Mutex::new(0)),
    };
    let store = InMemoryPersistenceStore::new();
    let handle = PersistenceHandle::from_store(store.clone());

    // Define v1.0 with 2 steps. We'll interrupt after 1st step.
    let axon_v10 = Axon::<u32, u32, TestError, ()>::new("ActiveNodeTest")
        .with_version("v1.0")
        .then(counter_v10.clone()) // Node index 1
        .then(counter_v10.clone()); // Node index 2

    let mut bus_v10 = Bus::new();
    bus_v10.insert(handle.clone());
    bus_v10.insert(ranvier_runtime::persistence::PersistenceTraceId::new(
        "trace-active",
    ));

    // We need to manually persist an event for node 1 so it looks like it finished node 1.
    // In a real scenario, this happens naturally.
    // But since axon.execute runs to completion, we'll just manually prime the store.
    let node1_id = axon_v10.schematic.nodes[1].id.clone();

    // Prime the store: Trace finished node 1
    ranvier_runtime::axon::persist_execution_event(
        &handle,
        "trace-active",
        "ActiveNodeTest",
        "v1.0",
        1,
        Some(node1_id.clone()),
        "Next",
        Some(serde_json::json!(1)),
    )
    .await;

    // Define v1.1. Same structure but different IDs.
    let counter_v11 = StepCounter {
        count: Arc::new(Mutex::new(0)),
    };
    let axon_v11 = Axon::<u32, u32, TestError, ()>::new("ActiveNodeTest")
        .with_version("v1.1")
        .then(counter_v11.clone()) // Node index 1
        .then(counter_v11.clone()); // Node index 2

    let node1_v11_id = axon_v11.schematic.nodes[1].id.clone();
    let node2_v11_id = axon_v11.schematic.nodes[2].id.clone();

    // Register migration: MigrateActiveNode from v1.0 node1 to v1.1 node2 (skipped node 1)
    let mut registry = MigrationRegistry::new("ActiveNodeTest");
    let mut mapping = std::collections::HashMap::new();
    mapping.insert(
        node1_id.clone(),
        MigrationStrategy::MigrateActiveNode {
            old_node_id: node1_id,
            new_node_id: node2_v11_id.clone(),
        },
    );

    registry.register(SnapshotMigration {
        name: Some("Mapping migration".to_string()),
        from_version: "v1.0".to_string(),
        to_version: "v1.1".to_string(),
        default_strategy: MigrationStrategy::Fail,
        node_mapping: mapping,
        payload_mapper: None,
    });

    let mut bus_v11 = Bus::new();
    bus_v11.insert(handle.clone());
    bus_v11.insert(ranvier_runtime::persistence::PersistenceTraceId::new(
        "trace-active",
    ));
    bus_v11.insert(registry);

    // Execute v1.1. It should find node 1 was last, map it to node 2, and start node 2.
    let _ = axon_v11.execute(0, &(), &mut bus_v11).await;

    // Counter for v11 should be 1 (only node 2 ran)
    {
        let count = *counter_v11.count.lock().await;
        assert_eq!(count, 1);
    }
}

#[tokio::test]
async fn test_migration_fallback_to_node() {
    let counter_v10 = StepCounter {
        count: Arc::new(Mutex::new(0)),
    };
    let store = InMemoryPersistenceStore::new();
    let handle = PersistenceHandle::from_store(store.clone());

    let axon_v10 = Axon::<u32, u32, TestError, ()>::new("FallbackTest")
        .with_version("v1.0")
        .then(counter_v10.clone());

    let node1_id = axon_v10.schematic.nodes[1].id.clone();

    // Prime the store: Trace finished node 1
    ranvier_runtime::axon::persist_execution_event(
        &handle,
        "trace-fallback",
        "FallbackTest",
        "v1.0",
        1,
        Some(node1_id.clone()),
        "Next",
        Some(serde_json::json!(1)),
    )
    .await;

    // Define v1.1.
    let counter_v11 = StepCounter {
        count: Arc::new(Mutex::new(0)),
    };
    let axon_v11 = Axon::<u32, u32, TestError, ()>::new("FallbackTest")
        .with_version("v1.1")
        .then(counter_v11.clone()) // Node 1
        .then(counter_v11.clone()); // Node 2

    let node2_v11_id = axon_v11.schematic.nodes[2].id.clone();

    // Register migration: FallbackToNode(node2)
    let mut registry = MigrationRegistry::new("FallbackTest");
    registry.register(SnapshotMigration {
        name: Some("Fallback migration".to_string()),
        from_version: "v1.0".to_string(),
        to_version: "v1.1".to_string(),
        default_strategy: MigrationStrategy::FallbackToNode(node1_id.clone()), // Use explicit fallback
        node_mapping: {
            let mut m = std::collections::HashMap::new();
            m.insert(node1_id, MigrationStrategy::FallbackToNode(node2_v11_id));
            m
        },
        payload_mapper: None,
    });

    let mut bus_v11 = Bus::new();
    bus_v11.insert(handle.clone());
    bus_v11.insert(ranvier_runtime::persistence::PersistenceTraceId::new(
        "trace-fallback",
    ));
    bus_v11.insert(registry);

    // Execute v1.1. It should ignore v1.0 and jump to v1.1 node 2.
    let _ = axon_v11.execute(0, &(), &mut bus_v11).await;

    // Counter for v11 should be 1 (only node 2 ran)
    {
        let count = *counter_v11.count.lock().await;
        assert_eq!(count, 1);
    }
}

// --- Multi-hop migration and event-sourcing replay tests ---

/// Mapper that renames a field: { "user_id": N } → { "user_id": "u-N" }
struct UserIdToStringMapper;
impl SchemaMigrationMapper for UserIdToStringMapper {
    fn map_state(&self, old: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let mut v = old.clone();
        if let Some(uid) = v.get("user_id").and_then(|v| v.as_u64()) {
            v["user_id"] = serde_json::json!(format!("u-{}", uid));
        }
        Ok(v)
    }
}

/// Mapper that adds a "migrated" flag.
struct AddMigratedFlagMapper;
impl SchemaMigrationMapper for AddMigratedFlagMapper {
    fn map_state(&self, old: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let mut v = old.clone();
        v["migrated"] = serde_json::json!(true);
        Ok(v)
    }
}

#[tokio::test]
async fn test_multi_hop_migration_v10_to_v12_with_payload_evolution() {
    let store = InMemoryPersistenceStore::new();
    let handle = PersistenceHandle::from_store(store.clone());

    // Prime a v1.0 trace with payload { "user_id": 42 }
    let counter = StepCounter {
        count: Arc::new(Mutex::new(0)),
    };
    let axon_v10 = Axon::<u32, u32, TestError, ()>::new("MultiHopTest")
        .with_version("v1.0")
        .then(counter.clone());

    let node1_id = axon_v10.schematic.nodes[1].id.clone();

    ranvier_runtime::axon::persist_execution_event(
        &handle,
        "trace-multihop",
        "MultiHopTest",
        "v1.0",
        1,
        Some(node1_id.clone()),
        "Next",
        Some(serde_json::json!({ "user_id": 42 })),
    )
    .await;

    // Register multi-hop migration: v1.0 → v1.1 → v1.2
    let mut registry = MigrationRegistry::new("MultiHopTest");

    // v1.0 → v1.1: Convert user_id from int to string
    registry.register(SnapshotMigration {
        name: Some("v1.0 to v1.1: user_id int→string".to_string()),
        from_version: "v1.0".to_string(),
        to_version: "v1.1".to_string(),
        default_strategy: MigrationStrategy::ResumeFromStart,
        node_mapping: std::collections::HashMap::new(),
        payload_mapper: Some(Arc::new(UserIdToStringMapper)),
    });

    // v1.1 → v1.2: Add migrated flag
    registry.register(SnapshotMigration {
        name: Some("v1.1 to v1.2: add migrated flag".to_string()),
        from_version: "v1.1".to_string(),
        to_version: "v1.2".to_string(),
        default_strategy: MigrationStrategy::ResumeFromStart,
        node_mapping: std::collections::HashMap::new(),
        payload_mapper: Some(Arc::new(AddMigratedFlagMapper)),
    });

    // Use replay_and_recover to verify multi-hop
    let result = replay_and_recover(&store, "trace-multihop", "v1.2", &registry)
        .await
        .unwrap();

    assert_eq!(result.original_version, "v1.0");
    assert_eq!(result.target_version, "v1.2");
    assert_eq!(result.migration_hops.len(), 2);
    assert_eq!(
        result.migration_hops[0],
        ("v1.0".to_string(), "v1.1".to_string())
    );
    assert_eq!(
        result.migration_hops[1],
        ("v1.1".to_string(), "v1.2".to_string())
    );

    // Verify payload was transformed through both mappers
    let payload = result.recovered_payload.unwrap();
    assert_eq!(payload["user_id"], serde_json::json!("u-42"));
    assert_eq!(payload["migrated"], serde_json::json!(true));
}

#[tokio::test]
async fn test_replay_and_recover_no_migration_needed() {
    let store = InMemoryPersistenceStore::new();
    let handle = PersistenceHandle::from_store(store.clone());

    let counter = StepCounter {
        count: Arc::new(Mutex::new(0)),
    };
    let axon = Axon::<u32, u32, TestError, ()>::new("SameVersionTest")
        .with_version("v2.0")
        .then(counter.clone());

    ranvier_runtime::axon::persist_execution_event(
        &handle,
        "trace-same",
        "SameVersionTest",
        "v2.0",
        1,
        Some(axon.schematic.nodes[1].id.clone()),
        "Next",
        Some(serde_json::json!(100)),
    )
    .await;

    let registry = MigrationRegistry::new("SameVersionTest");
    let result = replay_and_recover(&store, "trace-same", "v2.0", &registry)
        .await
        .unwrap();

    assert_eq!(result.original_version, "v2.0");
    assert_eq!(result.target_version, "v2.0");
    assert!(result.migration_hops.is_empty());
    assert_eq!(result.recovered_payload.unwrap(), serde_json::json!(100));
}

#[tokio::test]
async fn test_replay_and_recover_no_path_fails() {
    let store = InMemoryPersistenceStore::new();
    let handle = PersistenceHandle::from_store(store.clone());

    let counter = StepCounter {
        count: Arc::new(Mutex::new(0)),
    };
    let axon = Axon::<u32, u32, TestError, ()>::new("NoPathTest")
        .with_version("v1.0")
        .then(counter.clone());

    ranvier_runtime::axon::persist_execution_event(
        &handle,
        "trace-nopath",
        "NoPathTest",
        "v1.0",
        1,
        Some(axon.schematic.nodes[1].id.clone()),
        "Next",
        Some(serde_json::json!(1)),
    )
    .await;

    let registry = MigrationRegistry::new("NoPathTest");
    // No migrations registered; requesting v3.0 should fail
    let result = replay_and_recover(&store, "trace-nopath", "v3.0", &registry).await;
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("no migration path")
    );
}

#[tokio::test]
async fn test_multi_hop_axon_execution_with_payload_mapping() {
    let store = InMemoryPersistenceStore::new();
    let handle = PersistenceHandle::from_store(store.clone());

    // Prime v1.0 trace
    let counter_v10 = StepCounter {
        count: Arc::new(Mutex::new(0)),
    };
    let axon_v10 = Axon::<u32, u32, TestError, ()>::new("AxonMultiHop")
        .with_version("v1.0")
        .then(counter_v10.clone());

    let node1_id = axon_v10.schematic.nodes[1].id.clone();

    ranvier_runtime::axon::persist_execution_event(
        &handle,
        "trace-axon-mh",
        "AxonMultiHop",
        "v1.0",
        1,
        Some(node1_id.clone()),
        "Next",
        Some(serde_json::json!({ "user_id": 99 })),
    )
    .await;

    // Build v1.2 axon
    let counter_v12 = StepCounter {
        count: Arc::new(Mutex::new(0)),
    };
    let axon_v12 = Axon::<u32, u32, TestError, ()>::new("AxonMultiHop")
        .with_version("v1.2")
        .then(counter_v12.clone())
        .then(counter_v12.clone())
        .then(counter_v12.clone());

    // Register multi-hop: v1.0 → v1.1 → v1.2
    let mut registry = MigrationRegistry::new("AxonMultiHop");
    registry.register(SnapshotMigration {
        name: Some("v1.0→v1.1".to_string()),
        from_version: "v1.0".to_string(),
        to_version: "v1.1".to_string(),
        default_strategy: MigrationStrategy::ResumeFromStart,
        node_mapping: std::collections::HashMap::new(),
        payload_mapper: Some(Arc::new(UserIdToStringMapper)),
    });
    registry.register(SnapshotMigration {
        name: Some("v1.1→v1.2".to_string()),
        from_version: "v1.1".to_string(),
        to_version: "v1.2".to_string(),
        default_strategy: MigrationStrategy::ResumeFromStart,
        node_mapping: std::collections::HashMap::new(),
        payload_mapper: Some(Arc::new(AddMigratedFlagMapper)),
    });

    let mut bus = Bus::new();
    bus.insert(handle.clone());
    bus.insert(ranvier_runtime::persistence::PersistenceTraceId::new(
        "trace-axon-mh",
    ));
    bus.insert(registry);

    // Execute v1.2; should apply multi-hop migration and ResumeFromStart
    let _ = axon_v12.execute(0, &(), &mut bus).await;

    // All 3 v1.2 steps should have run (ResumeFromStart means start_step=0)
    {
        let count = *counter_v12.count.lock().await;
        assert_eq!(count, 3);
    }
}
