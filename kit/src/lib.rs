//! Ranvier facade crate.
//!
//! This crate re-exports core, runtime, http, and std crates with a single entry point.
//! `Ranvier::http()` remains an ingress builder, not a web server.
//!
//! For design philosophy, see [PHILOSOPHY.md](../docs/PHILOSOPHY.md).
//! For architecture decisions, see [DESIGN_PRINCIPLES.md](../docs/DESIGN_PRINCIPLES.md).

pub use ranvier_core as core;
#[cfg(feature = "guard")]
pub use ranvier_guard as guard;
#[cfg(feature = "http")]
pub use ranvier_http as http;
pub use ranvier_macros as macros;
#[cfg(feature = "openapi")]
pub use ranvier_openapi as openapi;
pub use ranvier_runtime as runtime;
#[cfg(feature = "std")]
pub use ranvier_std as std;

// Derive macros re-export (proc-macros require direct re-export)
pub use ranvier_macros::ResourceRequirement;

// AuthContext and AuthScheme live in ranvier-core::iam (always available, no feature gate).
pub use ranvier_core::iam::{AuthContext, AuthScheme};
pub use ranvier_core::tenant::{IsolationPolicy, TenantExtractor, TenantId, TenantResolver};
pub use ranvier_core::{Bus, Never, Outcome, Schematic, Transition};
#[cfg(feature = "http")]
pub use ranvier_http::{HttpIngress, Ranvier, RanvierService};
#[cfg(feature = "openapi")]
pub use ranvier_openapi::{OpenApiDocument, OpenApiGenerator, swagger_ui_html};
pub use ranvier_runtime::Axon;

pub mod prelude {
    pub use ranvier_core::prelude::*;
    #[cfg(feature = "guard")]
    pub use ranvier_guard::prelude::*;
    pub use ranvier_macros::ResourceRequirement;
    #[cfg(feature = "http")]
    pub use ranvier_http::prelude::*;
    #[cfg(feature = "openapi")]
    pub use ranvier_openapi::prelude::*;
    pub use ranvier_runtime::prelude::*;
    #[cfg(feature = "std")]
    pub use ranvier_std::prelude::*;
}
