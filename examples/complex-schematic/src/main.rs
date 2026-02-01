use anyhow::Result;
use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_core::schematic::{Edge, EdgeType, Node, NodeKind};
use serde::{Deserialize, Serialize};

// --- Data Types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LoginInput {
    username: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UserContext {
    user_id: String,
    role: String,
}

// --- Transitions ---

#[derive(Clone)]
struct Authenticate;

#[async_trait]
impl Transition<LoginInput, UserContext> for Authenticate {
    type Error = anyhow::Error;

    async fn run(&self, input: LoginInput, _bus: &mut Bus) -> Outcome<UserContext, Self::Error> {
        if input.username == "admin" {
            Outcome::Next(UserContext {
                user_id: "u1".to_string(),
                role: "admin".to_string(),
            })
        } else {
            // In a real app, this would be a Branch or Emit
            // For now, let's simulate a Branch return
            Outcome::Branch(
                "LoginFailed".to_string(),
                Some(serde_json::json!("Invalid credentials")),
            )
        }
    }
}

// --- Main ---

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== Complex Schematic Extraction Demo ===\n");

    let input = LoginInput {
        username: "guest".to_string(),
    };

    // 1. Build the Linear Axon
    // Axon currently builds the 'Happy Path' automatically
    let mut axon = Axon::start(input, "StartFlow").then(Authenticate);

    // 2. Start Manual Schematic Enhancement
    // Since Axon's Builder doesn't yet support auto-extraction of Branches,
    // we manually inject the structural knowledge here to demonstrate the Schematic's capability.

    // Find the 'Authenticate' node (it's the last one)
    let auth_node_id = axon.schematic.nodes.last().unwrap().id.clone();

    // Create a 'LoginFailed' Node (Synapse/Branch target)
    let fail_node_id = uuid::Uuid::new_v4().to_string();
    let fail_node = Node {
        id: fail_node_id.clone(),
        kind: NodeKind::Egress,
        label: "LoginFailedHandler".to_string(),
        input_type: "String".to_string(), // Error message
        output_type: "Void".to_string(),
        metadata: Default::default(),
        source_location: None,
    };

    // Create a Branch Edge
    let fail_edge = Edge {
        from: auth_node_id,
        to: fail_node_id,
        kind: EdgeType::Branch("LoginFailed".to_string()),
        label: Some("On Failure".to_string()),
    };

    // Create a Subgraph Node (to demonstrate nesting)
    let subgraph_id = uuid::Uuid::new_v4().to_string();
    let sub_schematic = ranvier_core::schematic::Schematic::new("AuditSubFlow");
    let subgraph_node = Node {
        id: subgraph_id.clone(),
        kind: NodeKind::Subgraph(Box::new(sub_schematic)),
        label: "AuditProcess".to_string(),
        input_type: "UserContext".to_string(),
        output_type: "Void".to_string(),
        metadata: Default::default(),
        source_location: None,
    };

    // Add Subgraph to the main graph (conceptually unconnected for now, just to show JSON structure)
    axon.schematic.nodes.push(fail_node);
    axon.schematic.edges.push(fail_edge);
    axon.schematic.nodes.push(subgraph_node);

    // 3. Export JSON
    let json = serde_json::to_string_pretty(&axon.schematic)?;
    println!("{}", json);

    Ok(())
}
