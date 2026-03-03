pub mod bus;
pub mod cluster;
pub mod debug;
pub mod event;
pub mod metadata;
pub mod outcome;
pub mod policy;
pub mod saga;
pub mod schematic;
pub mod static_gen;
pub mod synapse;
pub mod telemetry;
pub mod timeline;
pub mod transition;

// NOTE: service module moved to ranvier-http (Discussion 190: Protocol-agnostic Core)
// For Ingress adapters, use: ranvier_http

// Static generation exports
pub use static_gen::{
    StaticAxon, StaticBuildConfig, StaticBuildResult, StaticManifest, StaticNode, StaticStateEntry,
    read_json_file, write_json_file,
};

// Prelude module for convenient imports
pub mod prelude {
    pub use crate::bus::{Bus, BusAccessError, BusAccessPolicy, BusTypeRef};
    pub use crate::debug::{DebugControl, DebugState};
    pub use crate::event::{DeadLetter, DlqPolicy, DlqReader, DlqSink, EventSink, EventSource};
    pub use crate::policy::{DynamicPolicy, PolicyRegistry};
    pub use crate::metadata::StepMetadata;
    pub use crate::outcome::{BranchId, NodeId, Outcome};
    pub use crate::saga::{SagaCompensationRegistry, SagaPolicy, SagaStack, SagaTask};
    pub use crate::schematic::{Edge, Node, NodeKind, Schematic, SchemaMigrationMapper};
    pub use crate::timeline::{Timeline, TimelineEvent};
    pub use crate::transition::Transition;
}

// Legacy modules removed/deprecated
// pub mod module;
// pub mod circuit;
// pub mod service; // Moved to ranvier-http

pub use bus::{Bus, BusAccessError, BusAccessPolicy, BusTypeRef};
pub use cluster::{ClusterBus, ClusterError, DistributedLock};
pub use outcome::Outcome;
pub use schematic::Schematic;
pub use timeline::{Timeline, TimelineEvent};
pub use transition::Transition;

/// Build a `Bus` with optional resource inserts in one expression.
///
/// This macro is a helper for repetitive example/test wiring and does not
/// change Bus boundary semantics: resources remain explicit values.
#[macro_export]
macro_rules! ranvier_bus {
    () => {{
        $crate::bus::Bus::new()
    }};
    ($($resource:expr),+ $(,)?) => {{
        let mut __ranvier_bus = $crate::bus::Bus::new();
        $(
            __ranvier_bus.insert($resource);
        )+
        __ranvier_bus
    }};
}

#[cfg(test)]
mod macro_tests {
    #[test]
    fn ranvier_bus_macro_creates_empty_bus() {
        let bus = crate::ranvier_bus!();
        assert!(bus.is_empty());
    }

    #[test]
    fn ranvier_bus_macro_inserts_multiple_resources() {
        let bus = crate::ranvier_bus!(42i32, String::from("value"));
        assert_eq!(*bus.read::<i32>().unwrap(), 42);
        assert_eq!(bus.read::<String>().unwrap(), "value");
    }
}
