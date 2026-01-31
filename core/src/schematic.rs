use crate::metadata::StepMetadata;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 스키마 버전 상수
pub const SCHEMA_VERSION: &str = "1.0";

/// The Static Analysis View of a Circuit.
///
/// `Schematic` is the graph representation extracted from the Axon Builder.
/// It is used for visualization, documentation, and verification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schematic {
    /// 스키마 버전 (호환성 체크용)
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
pub struct Node {
    pub id: String, // Uuid typically
    pub kind: NodeKind,
    pub label: String,
    pub input_type: String,
    pub output_type: String, // Primary output type for Next
    pub metadata: StepMetadata,
    /// 소스 코드 위치 (Studio Code↔Node 매핑용)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_location: Option<SourceLocation>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schematic_default_has_version_and_id() {
        let schematic = Schematic::new("Test Circuit");
        assert_eq!(schematic.schema_version, SCHEMA_VERSION);
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
}
