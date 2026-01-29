use crate::metadata::StepMetadata;
use serde::{Deserialize, Serialize};

/// The Static Analysis View of a Circuit.
///
/// `Schematic` is the graph representation extracted from the Axon Builder.
/// It is used for visualization, documentation, and verification.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Schematic {
    pub name: String,
    pub description: Option<String>,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

impl Schematic {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
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
