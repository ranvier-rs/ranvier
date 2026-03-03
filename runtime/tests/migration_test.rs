use ranvier_core::prelude::*;
use ranvier_core::schematic::{MigrationRegistry, SnapshotMigration, MigrationStrategy};
use ranvier_runtime::Axon;
use ranvier_runtime::persistence::{InMemoryPersistenceStore, PersistenceHandle};
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
    let counter = StepCounter { count: Arc::new(Mutex::new(0)) };
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
    bus.insert(ranvier_runtime::persistence::PersistenceTraceId::new("trace-1"));

    let _ = axon_v10.execute(0, &(), &mut bus).await;
    
    // Check counter
    {
        let count = *counter.count.lock().await;
        assert_eq!(count, 2);
    }

    // Now define v1.1
    let counter_v11 = StepCounter { count: Arc::new(Mutex::new(0)) }; // Reset counter for new run
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
    });

    let mut bus_v11 = Bus::new();
    bus_v11.insert(handle.clone());
    bus_v11.insert(ranvier_runtime::persistence::PersistenceTraceId::new("trace-1"));
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
        .then(StepCounter { count: Arc::new(Mutex::new(0)) });

    let mut bus = Bus::new();
    bus.insert(handle.clone());
    bus.insert(ranvier_runtime::persistence::PersistenceTraceId::new("trace-fail"));
    let _ = axon_v10.execute(0, &(), &mut bus).await;

    let axon_v11 = Axon::<u32, u32, TestError, ()>::new("FailTest")
        .with_version("v1.1")
        .then(StepCounter { count: Arc::new(Mutex::new(0)) });

    let mut bus_v11 = Bus::new();
    bus_v11.insert(handle.clone());
    bus_v11.insert(ranvier_runtime::persistence::PersistenceTraceId::new("trace-fail"));
    // No migration registered

    let outcome = axon_v11.execute(0, &(), &mut bus_v11).await;
    
    // Should be Outcome::Emit with version_mismatch_failed (based on axon.rs:870)
    match outcome {
        Outcome::Emit(event, _) => assert_eq!(event, "execution.resumption.version_mismatch_failed"),
        _ => panic!("expected version mismatch failure, got {:?}", outcome),
    }
}

#[tokio::test]
async fn test_migration_migrate_active_node() {
    let counter_v10 = StepCounter { count: Arc::new(Mutex::new(0)) };
    let store = InMemoryPersistenceStore::new();
    let handle = PersistenceHandle::from_store(store.clone());

    // Define v1.0 with 2 steps. We'll interrupt after 1st step.
    let axon_v10 = Axon::<u32, u32, TestError, ()>::new("ActiveNodeTest")
        .with_version("v1.0")
        .then(counter_v10.clone()) // Node index 1
        .then(counter_v10.clone()); // Node index 2

    let mut bus_v10 = Bus::new();
    bus_v10.insert(handle.clone());
    bus_v10.insert(ranvier_runtime::persistence::PersistenceTraceId::new("trace-active"));
    
    // We need to manually persist an event for node 1 so it looks like it finished node 1.
    // In a real scenario, this happens naturally.
    // But since axon.execute runs to completion, we'll just manually prime the store.
    let node1_id = axon_v10.schematic.nodes[1].id.clone();
    
    // Prime the store: Trace finished node 1
    ranvier_runtime::axon::persist_execution_event(
        &handle, "trace-active", "ActiveNodeTest", "v1.0", 1, Some(node1_id.clone()), "Next", Some(serde_json::json!(1))
    ).await;

    // Define v1.1. Same structure but different IDs.
    let counter_v11 = StepCounter { count: Arc::new(Mutex::new(0)) };
    let axon_v11 = Axon::<u32, u32, TestError, ()>::new("ActiveNodeTest")
        .with_version("v1.1")
        .then(counter_v11.clone()) // Node index 1
        .then(counter_v11.clone()); // Node index 2

    let node1_v11_id = axon_v11.schematic.nodes[1].id.clone();
    let node2_v11_id = axon_v11.schematic.nodes[2].id.clone();

    // Register migration: MigrateActiveNode from v1.0 node1 to v1.1 node2 (skipped node 1)
    let mut registry = MigrationRegistry::new("ActiveNodeTest");
    let mut mapping = std::collections::HashMap::new();
    mapping.insert(node1_id.clone(), MigrationStrategy::MigrateActiveNode { 
        old_node_id: node1_id, 
        new_node_id: node2_v11_id.clone() 
    });

    registry.register(SnapshotMigration {
        name: Some("Mapping migration".to_string()),
        from_version: "v1.0".to_string(),
        to_version: "v1.1".to_string(),
        default_strategy: MigrationStrategy::Fail,
        node_mapping: mapping,
    });

    let mut bus_v11 = Bus::new();
    bus_v11.insert(handle.clone());
    bus_v11.insert(ranvier_runtime::persistence::PersistenceTraceId::new("trace-active"));
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
    let counter_v10 = StepCounter { count: Arc::new(Mutex::new(0)) };
    let store = InMemoryPersistenceStore::new();
    let handle = PersistenceHandle::from_store(store.clone());

    let axon_v10 = Axon::<u32, u32, TestError, ()>::new("FallbackTest")
        .with_version("v1.0")
        .then(counter_v10.clone());
    
    let node1_id = axon_v10.schematic.nodes[1].id.clone();
    
    // Prime the store: Trace finished node 1
    ranvier_runtime::axon::persist_execution_event(
        &handle, "trace-fallback", "FallbackTest", "v1.0", 1, Some(node1_id.clone()), "Next", Some(serde_json::json!(1))
    ).await;

    // Define v1.1. 
    let counter_v11 = StepCounter { count: Arc::new(Mutex::new(0)) };
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
    });

    let mut bus_v11 = Bus::new();
    bus_v11.insert(handle.clone());
    bus_v11.insert(ranvier_runtime::persistence::PersistenceTraceId::new("trace-fallback"));
    bus_v11.insert(registry);

    // Execute v1.1. It should ignore v1.0 and jump to v1.1 node 2.
    let _ = axon_v11.execute(0, &(), &mut bus_v11).await;

    // Counter for v11 should be 1 (only node 2 ran)
    {
        let count = *counter_v11.count.lock().await;
        assert_eq!(count, 1);
    }
}
