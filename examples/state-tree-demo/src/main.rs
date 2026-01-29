use ranvier_core::prelude::*;
use ranvier_core::static_gen::StaticNode;
use serde::Serialize;

// Define a sample node implementing StaticNode
struct AuthNode {
    id: &'static str,
    next: &'static str,
}

impl StaticNode for AuthNode {
    fn id(&self) -> &'static str {
        self.id
    }

    fn kind(&self) -> NodeKind {
        NodeKind::Atom
    }

    fn next_nodes(&self) -> Vec<&'static str> {
        vec![self.next]
    }
}

// Another sample node
struct DbNode {
    id: &'static str,
}

impl StaticNode for DbNode {
    fn id(&self) -> &'static str {
        self.id
    }

    fn kind(&self) -> NodeKind {
        NodeKind::Atom
    }

    fn next_nodes(&self) -> Vec<&'static str> {
        vec![] // Terminal node for this linear segment
    }
}

// Structure to hold the static tree
#[derive(Serialize)]
struct StaticTree {
    nodes: Vec<StaticNodeView>,
}

#[derive(Serialize)]
struct StaticNodeView {
    id: String,
    kind: NodeKind,
    next: Vec<String>,
}

impl StaticNodeView {
    fn from_static<T: StaticNode>(node: &T) -> Self {
        Self {
            id: node.id().to_string(),
            kind: node.kind(),
            next: node.next_nodes().iter().map(|s| s.to_string()).collect(),
        }
    }
}

fn main() {
    // Define the graph topology statically
    let db_node = DbNode { id: "db-01" };
    let auth_node = AuthNode {
        id: "auth-01",
        next: "db-01",
    };

    // Collect into a tree representation
    let tree = StaticTree {
        nodes: vec![
            StaticNodeView::from_static(&auth_node),
            StaticNodeView::from_static(&db_node),
        ],
    };

    // Export to JSON
    let json = serde_json::to_string_pretty(&tree).unwrap();
    println!("{}", json);
}
