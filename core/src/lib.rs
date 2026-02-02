pub mod axon;
pub mod bus;
pub mod event;
pub mod metadata;
pub mod outcome;
pub mod replay;
pub mod schematic;
pub mod static_gen;
pub mod synapse;
pub mod telemetry;
pub mod timeline;
pub mod transition;

// NOTE: service module moved to ranvier-http (Discussion 190: Protocol-agnostic Core)
// For RanvierService, use: ranvier_http::RanvierService

// Static generation exports
pub use static_gen::{
    read_json_file, write_json_file, StaticAxon, StaticBuildConfig, StaticBuildResult,
    StaticManifest, StaticNode, StaticStateEntry,
};

// Prelude module for convenient imports
pub mod prelude {
    pub use crate::axon::Axon;
    pub use crate::bus::Bus;
    pub use crate::event::{EventSink, EventSource};
    pub use crate::metadata::StepMetadata;
    pub use crate::outcome::{BranchId, NodeId, Outcome};
    pub use crate::schematic::{Edge, Node, NodeKind, Schematic};
    pub use crate::transition::Transition;
}

// Legacy modules removed/deprecated
// pub mod module;
// pub mod circuit;
// pub mod service; // Moved to ranvier-http

pub use axon::Axon;
pub use bus::Bus;
pub use outcome::Outcome;
pub use replay::ReplayEngine;
pub use schematic::Schematic;
pub use transition::Transition;
