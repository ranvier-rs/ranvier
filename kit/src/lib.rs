//! Ranvier facade crate.
//!
//! This crate re-exports core, runtime, http, and std crates with a single entry point.
//! `Ranvier::http()` remains an ingress builder, not a web server.

#[cfg(feature = "auth")]
pub use ranvier_auth as auth;
pub use ranvier_core as core;
#[cfg(feature = "guard")]
pub use ranvier_guard as guard;
#[cfg(feature = "http")]
pub use ranvier_http as http;
#[cfg(feature = "openapi")]
pub use ranvier_openapi as openapi;
pub use ranvier_runtime as runtime;
#[cfg(feature = "std")]
pub use ranvier_std as std;

#[cfg(feature = "auth")]
pub use ranvier_auth::{
    ApiKeyAuthLayer, AuthPolicy, BearerAuthLayer, RequireRoleLayer,
};
// AuthContext and AuthScheme now live in ranvier-core::iam (always available, no feature gate).
pub use ranvier_core::iam::{AuthContext, AuthScheme};
pub use ranvier_core::tenant::{IsolationPolicy, TenantExtractor, TenantId, TenantResolver};
pub use ranvier_core::{Bus, Never, Outcome, Schematic, Transition};
#[cfg(feature = "guard")]
pub use ranvier_guard::{
    CorsGuardLayer, RateLimitLayer, RateLimitPolicy, SecurityHeadersLayer, SecurityHeadersPolicy,
};
#[cfg(feature = "http")]
pub use ranvier_http::{HttpIngress, Ranvier, RanvierService};
#[cfg(feature = "openapi")]
pub use ranvier_openapi::{OpenApiDocument, OpenApiGenerator, swagger_ui_html};
pub use ranvier_runtime::Axon;

pub mod prelude {
    #[cfg(feature = "auth")]
    pub use ranvier_auth::prelude::*;
    pub use ranvier_core::prelude::*;
    #[cfg(feature = "guard")]
    pub use ranvier_guard::prelude::*;
    #[cfg(feature = "http")]
    pub use ranvier_http::prelude::*;
    #[cfg(feature = "openapi")]
    pub use ranvier_openapi::prelude::*;
    pub use ranvier_runtime::prelude::*;
    #[cfg(feature = "std")]
    pub use ranvier_std::prelude::*;
}
