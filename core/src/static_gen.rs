use crate::schematic::NodeKind;

pub trait StaticNode {
    /// Unique identifier for the node
    fn id(&self) -> &'static str;

    /// The kind of node (Start, Process, etc.)
    fn kind(&self) -> NodeKind;

    /// List of IDs of nodes this node connects to
    fn next_nodes(&self) -> Vec<&'static str>;
}
