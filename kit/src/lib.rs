//! Ranvier facade crate.
//!
//! This crate re-exports core, runtime, http, and std crates with a single entry point.
//! `Ranvier::http()` remains an ingress builder, not a web server.

pub use ranvier_core as core;
#[cfg(feature = "http")]
pub use ranvier_http as http;
pub use ranvier_runtime as runtime;
#[cfg(feature = "std")]
pub use ranvier_std as std;

pub use ranvier_core::{Bus, Outcome, Schematic, Transition};
#[cfg(feature = "http")]
pub use ranvier_http::{HttpIngress, Ranvier, RanvierService};
pub use ranvier_runtime::Axon;

pub mod prelude {
    pub use ranvier_core::prelude::*;
    pub use ranvier_runtime::prelude::*;
    #[cfg(feature = "http")]
    pub use ranvier_http::prelude::*;
    #[cfg(feature = "std")]
    pub use ranvier_std::prelude::*;
}
