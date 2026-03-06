use crate::metadata::StepMetadata;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// 스키마 버전 상수
pub const SCHEMA_VERSION: &str = "1.0";

fn default_schema_version() -> String {
    SCHEMA_VERSION.to_string()
}

fn parse_schema_version(version: &str) -> Option<(u64, u64)> {
    let trimmed = version.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut parts = trimmed.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor))
}

/// Returns true when the provided schematic schema version is supported by this crate.
///
/// Compatibility is evaluated at major-version level.
pub fn is_supported_schema_version(version: &str) -> bool {
    let Some((major, _)) = parse_schema_version(version) else {
        return false;
    };
    let Some((supported_major, _)) = parse_schema_version(SCHEMA_VERSION) else {
        return false;
    };
    major == supported_major
}

/// The Static Analysis View of a Circuit.
///
/// `Schematic` is the graph representation extracted from the Axon Builder.
/// It is used for visualization, documentation, and verification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schematic {
    /// 스키마 버전 (호환성 체크용)
    #[serde(default = "default_schema_version")]
    pub schema_version: String,
    /// Circuit 고유 식별자
    pub id: String,
    /// Circuit 이름
    pub name: String,
    /// 설명
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 생성 시각
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<DateTime<Utc>>,
    /// 노드 목록
    pub nodes: Vec<Node>,
    /// 엣지 목록
    pub edges: Vec<Edge>,
}

impl Default for Schematic {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION.to_string(),
            id: Uuid::new_v4().to_string(),
            name: String::new(),
            description: None,
            generated_at: Some(Utc::now()),
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }
}

impl Schematic {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }

    pub fn is_supported_schema_version(&self) -> bool {
        is_supported_schema_version(&self.schema_version)
    }

    /// 기존 ID를 유지하면서 새 Schematic 생성
    pub fn with_id(name: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            ..Default::default()
        }
    }
}

/// 소스 코드 위치 정보 (Studio Code↔Node 매핑용)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceLocation {
    /// 파일 경로 (프로젝트 루트 기준 상대 경로)
    pub file: String,
    /// 라인 번호 (1-indexed)
    pub line: u32,
    /// 컬럼 번호 (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
}

impl SourceLocation {
    pub fn new(file: impl Into<String>, line: u32) -> Self {
        Self {
            file: file.into(),
            line,
            column: None,
        }
    }

    pub fn with_column(file: impl Into<String>, line: u32, column: u32) -> Self {
        Self {
            file: file.into(),
            line,
            column: Some(column),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: String, // Uuid typically
    pub kind: NodeKind,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_type: String,
    pub output_type: String, // Primary output type for Next
    pub resource_type: String,
    pub metadata: StepMetadata,
    /// Optional transition-level Bus capability policy metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bus_capability: Option<BusCapabilitySchema>,
    /// 소스 코드 위치 (Studio Code↔Node 매핑용)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_location: Option<SourceLocation>,
    /// Visual position in schematic
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
    /// Schematic-level Saga compensation routing.
    /// Points to the node ID that handles compensation for this node.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compensation_node_id: Option<String>,
    /// JSON Schema for the node's input type.
    /// Populated via `.with_input_schema::<T>()` or `#[transition(schema)]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<serde_json::Value>,
    /// JSON Schema for the node's output type.
    /// Populated via `.with_output_schema::<T>()` or `#[transition(schema)]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BusCapabilitySchema {
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub allow: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub deny: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NodeKind {
    Ingress,                  // Handler / Start
    Atom,                     // Single action
    Synapse,                  // Connection point / Branch
    Egress,                   // Response / End
    Subgraph(Box<Schematic>), // Nested graph
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EdgeType {
    Linear,         // Outcome::Next
    Branch(String), // Outcome::Branch(id)
    Jump,           // Outcome::Jump
    Fault,          // Outcome::Fault
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub kind: EdgeType,
    pub label: Option<String>,
}

/// Defines how an in-flight workflow instance should be handled during a schema migration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MigrationStrategy {
    /// Stop and fail the in-flight instance.
    Fail,
    /// Wait for the instance to complete on the old version before migrating.
    CompleteOnOldVersion,
    /// Migrate the active node from the old ID to the new ID.
    MigrateActiveNode {
        old_node_id: String,
        new_node_id: String,
    },
    /// Resume from a specific fallback node.
    FallbackToNode(String),
    /// Abandon current state and resume from the Ingress node of the new version.
    ResumeFromStart,
}

/// Trait for transforming workflow payload between schema versions.
///
/// Implement this to define custom payload transformations when migrating
/// in-flight workflows across schematic versions.
pub trait SchemaMigrationMapper: Send + Sync {
    /// Transform the old state payload into the new version's expected format.
    fn map_state(&self, old_state: &serde_json::Value) -> anyhow::Result<serde_json::Value>;
}

/// A snapshot migration definition indicating how to move state from one schema version to another.
#[derive(Clone, Serialize, Deserialize)]
pub struct SnapshotMigration {
    /// Optional human-readable name for this migration.
    pub name: Option<String>,
    /// The unique identifier of the old schematic version.
    pub from_version: String,
    /// The unique identifier of the new schematic version.
    pub to_version: String,
    /// The default strategy to apply if a node-specific strategy is not provided.
    pub default_strategy: MigrationStrategy,
    /// Node-specific migration strategies, keyed by the active node ID in the old version.
    #[serde(skip_serializing_if = "HashMap::is_empty", default)]
    pub node_mapping: HashMap<String, MigrationStrategy>,
    /// Optional payload mapper for transforming state between versions.
    #[serde(skip)]
    pub payload_mapper: Option<std::sync::Arc<dyn SchemaMigrationMapper>>,
}

impl std::fmt::Debug for SnapshotMigration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SnapshotMigration")
            .field("name", &self.name)
            .field("from_version", &self.from_version)
            .field("to_version", &self.to_version)
            .field("default_strategy", &self.default_strategy)
            .field("node_mapping", &self.node_mapping)
            .field(
                "payload_mapper",
                &self.payload_mapper.as_ref().map(|_| ".."),
            )
            .finish()
    }
}

/// A registry of available snapshot migrations for a specific circuit.
///
/// Use this to look up how to move from a persisted version to the currently running version.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MigrationRegistry {
    pub circuit_id: String,
    pub migrations: Vec<SnapshotMigration>,
}

impl MigrationRegistry {
    pub fn new(circuit_id: impl Into<String>) -> Self {
        Self {
            circuit_id: circuit_id.into(),
            migrations: Vec::new(),
        }
    }

    pub fn register(&mut self, migration: SnapshotMigration) {
        self.migrations.push(migration);
    }

    /// Finds a direct migration from `from_version` to `to_version`.
    pub fn find_migration(&self, from: &str, to: &str) -> Option<&SnapshotMigration> {
        self.migrations
            .iter()
            .find(|m| m.from_version == from && m.to_version == to)
    }

    /// Finds a multi-hop migration path from `from_version` to `to_version`.
    ///
    /// Returns an ordered list of migrations to apply sequentially.
    /// Uses BFS to find the shortest path. Returns `None` if no path exists.
    pub fn find_migration_path(&self, from: &str, to: &str) -> Option<Vec<&SnapshotMigration>> {
        if from == to {
            return Some(Vec::new());
        }
        // Direct hop
        if let Some(direct) = self.find_migration(from, to) {
            return Some(vec![direct]);
        }
        // BFS for multi-hop
        let mut queue: std::collections::VecDeque<(String, Vec<usize>)> =
            std::collections::VecDeque::new();
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
        visited.insert(from.to_string());
        for (i, m) in self.migrations.iter().enumerate() {
            if m.from_version == from {
                visited.insert(m.to_version.clone());
                if m.to_version == to {
                    return Some(vec![&self.migrations[i]]);
                }
                queue.push_back((m.to_version.clone(), vec![i]));
            }
        }
        while let Some((current, path)) = queue.pop_front() {
            for (i, m) in self.migrations.iter().enumerate() {
                if m.from_version == current && !visited.contains(&m.to_version) {
                    let mut new_path = path.clone();
                    new_path.push(i);
                    if m.to_version == to {
                        return Some(new_path.iter().map(|&idx| &self.migrations[idx]).collect());
                    }
                    visited.insert(m.to_version.clone());
                    queue.push_back((m.to_version.clone(), new_path));
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schematic_default_has_version_and_id() {
        let schematic = Schematic::new("Test Circuit");
        assert_eq!(schematic.schema_version, SCHEMA_VERSION);
        assert!(schematic.is_supported_schema_version());
        assert!(!schematic.id.is_empty());
        assert!(schematic.generated_at.is_some());
    }

    #[test]
    fn test_schematic_serialization_with_new_fields() {
        let schematic = Schematic::new("Test");
        let json = serde_json::to_string_pretty(&schematic).unwrap();

        assert!(json.contains("schema_version"));
        assert!(json.contains("\"1.0\""));
        assert!(json.contains("generated_at"));
    }

    #[test]
    fn test_source_location_optional_in_json() {
        let schematic = Schematic::new("Test");
        let json = serde_json::to_string(&schematic).unwrap();

        // description과 source_location은 None이면 JSON에서 생략됨
        assert!(!json.contains("description"));
    }

    #[test]
    fn test_source_location_creation() {
        let loc = SourceLocation::new("src/main.rs", 42);
        assert_eq!(loc.file, "src/main.rs");
        assert_eq!(loc.line, 42);
        assert!(loc.column.is_none());

        let loc_with_col = SourceLocation::with_column("src/lib.rs", 10, 5);
        assert_eq!(loc_with_col.column, Some(5));
    }

    #[test]
    fn test_schema_version_defaults_when_missing_in_json() {
        let json = r#"{
            "id": "test-id",
            "name": "Legacy Schematic",
            "nodes": [],
            "edges": []
        }"#;
        let schematic: Schematic = serde_json::from_str(json).unwrap();
        assert_eq!(schematic.schema_version, SCHEMA_VERSION);
        assert!(schematic.is_supported_schema_version());
    }

    #[test]
    fn test_supported_schema_version_major_compatibility() {
        assert!(is_supported_schema_version("1"));
        assert!(is_supported_schema_version("1.0"));
        assert!(is_supported_schema_version("1.1"));
        assert!(is_supported_schema_version("1.0.9"));
        assert!(!is_supported_schema_version("2.0"));
        assert!(!is_supported_schema_version(""));
        assert!(!is_supported_schema_version("invalid"));
    }

    #[test]
    fn test_migration_registry_lookup() {
        let mut registry = MigrationRegistry::new("test-circuit");
        let migration = SnapshotMigration {
            name: Some("v1 to v2".to_string()),
            from_version: "1.0".to_string(),
            to_version: "2.0".to_string(),
            default_strategy: MigrationStrategy::Fail,
            node_mapping: HashMap::new(),
            payload_mapper: None,
        };
        registry.register(migration);

        let found = registry.find_migration("1.0", "2.0");
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, Some("v1 to v2".to_string()));

        let not_found = registry.find_migration("1.0", "3.0");
        assert!(not_found.is_none());
    }

    #[test]
    fn test_node_deserializes_without_schema_fields() {
        // RQ13: Backward compatibility — old JSON without input_schema/output_schema
        let json = r#"{
            "id": "node-1",
            "kind": "Atom",
            "label": "OldNode",
            "input_type": "i32",
            "output_type": "i32",
            "resource_type": "()",
            "metadata": {
                "id": "00000000-0000-0000-0000-000000000000",
                "label": "OldNode",
                "description": null,
                "inputs": [],
                "outputs": []
            }
        }"#;
        let node: Node = serde_json::from_str(json).unwrap();
        assert_eq!(node.label, "OldNode");
        assert!(node.input_schema.is_none());
        assert!(node.output_schema.is_none());
    }

    #[test]
    fn test_node_serializes_schema_fields_when_present() {
        let node = Node {
            id: "node-s".to_string(),
            kind: NodeKind::Atom,
            label: "WithSchema".to_string(),
            description: None,
            input_type: "MyInput".to_string(),
            output_type: "MyOutput".to_string(),
            resource_type: "()".to_string(),
            metadata: StepMetadata::default(),
            bus_capability: None,
            source_location: None,
            position: None,
            compensation_node_id: None,
            input_schema: Some(serde_json::json!({"type": "object"})),
            output_schema: Some(serde_json::json!({"type": "string"})),
        };
        let json = serde_json::to_value(&node).unwrap();
        assert_eq!(json["input_schema"], serde_json::json!({"type": "object"}));
        assert_eq!(json["output_schema"], serde_json::json!({"type": "string"}));
    }

    #[test]
    fn test_node_omits_schema_fields_when_none() {
        let node = Node {
            id: "node-n".to_string(),
            kind: NodeKind::Atom,
            label: "NoSchema".to_string(),
            description: None,
            input_type: "i32".to_string(),
            output_type: "i32".to_string(),
            resource_type: "()".to_string(),
            metadata: StepMetadata::default(),
            bus_capability: None,
            source_location: None,
            position: None,
            compensation_node_id: None,
            input_schema: None,
            output_schema: None,
        };
        let json = serde_json::to_value(&node).unwrap();
        let obj = json.as_object().unwrap();
        assert!(!obj.contains_key("input_schema"));
        assert!(!obj.contains_key("output_schema"));
    }

    #[test]
    fn test_schematic_with_schema_nodes_roundtrip() {
        let mut schematic = Schematic::new("SchemaTest");
        schematic.nodes.push(Node {
            id: "n1".to_string(),
            kind: NodeKind::Atom,
            label: "Step1".to_string(),
            description: None,
            input_type: "Request".to_string(),
            output_type: "Response".to_string(),
            resource_type: "()".to_string(),
            metadata: StepMetadata::default(),
            bus_capability: None,
            source_location: None,
            position: None,
            compensation_node_id: None,
            input_schema: Some(serde_json::json!({"type": "object", "required": ["name"]})),
            output_schema: None,
        });

        let json = serde_json::to_string(&schematic).unwrap();
        let deserialized: Schematic = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.nodes.len(), 1);
        assert!(deserialized.nodes[0].input_schema.is_some());
        assert!(deserialized.nodes[0].output_schema.is_none());
        assert_eq!(
            deserialized.nodes[0].input_schema.as_ref().unwrap()["required"][0],
            "name"
        );
    }

    #[test]
    fn test_legacy_schematic_json_deserializes() {
        // RQ13: Full schematic from pre-v0.20 (no input_schema/output_schema on nodes)
        let json = r#"{
            "schema_version": "1.0",
            "id": "legacy-1",
            "name": "LegacyCircuit",
            "nodes": [{
                "id": "n1",
                "kind": "Ingress",
                "label": "Start",
                "input_type": "String",
                "output_type": "String",
                "resource_type": "()",
                "metadata": {
                    "id": "00000000-0000-0000-0000-000000000000",
                    "label": "Start",
                    "description": null,
                    "inputs": [],
                    "outputs": []
                }
            }],
            "edges": []
        }"#;
        let schematic: Schematic = serde_json::from_str(json).unwrap();
        assert_eq!(schematic.name, "LegacyCircuit");
        assert_eq!(schematic.nodes.len(), 1);
        assert!(schematic.nodes[0].input_schema.is_none());
        assert!(schematic.nodes[0].output_schema.is_none());
    }

    #[test]
    fn test_multi_hop_migration_path() {
        let mut registry = MigrationRegistry::new("test-circuit");
        registry.register(SnapshotMigration {
            name: Some("v1→v2".to_string()),
            from_version: "1.0".to_string(),
            to_version: "2.0".to_string(),
            default_strategy: MigrationStrategy::ResumeFromStart,
            node_mapping: HashMap::new(),
            payload_mapper: None,
        });
        registry.register(SnapshotMigration {
            name: Some("v2→v3".to_string()),
            from_version: "2.0".to_string(),
            to_version: "3.0".to_string(),
            default_strategy: MigrationStrategy::ResumeFromStart,
            node_mapping: HashMap::new(),
            payload_mapper: None,
        });

        // Direct hop
        let path = registry.find_migration_path("1.0", "2.0").unwrap();
        assert_eq!(path.len(), 1);

        // Multi-hop
        let path = registry.find_migration_path("1.0", "3.0").unwrap();
        assert_eq!(path.len(), 2);
        assert_eq!(path[0].from_version, "1.0");
        assert_eq!(path[0].to_version, "2.0");
        assert_eq!(path[1].from_version, "2.0");
        assert_eq!(path[1].to_version, "3.0");

        // Same version (no-op)
        let path = registry.find_migration_path("1.0", "1.0").unwrap();
        assert!(path.is_empty());

        // No path
        let path = registry.find_migration_path("1.0", "4.0");
        assert!(path.is_none());
    }
}
