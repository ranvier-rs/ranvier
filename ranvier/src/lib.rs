// Re-export core modules
pub use ranvier_core::bus;
pub use ranvier_core::circuit;
pub use ranvier_core::metadata;
pub use ranvier_core::module;

pub mod response;
pub mod routing;
pub mod server;
#[path = "std/mod.rs"]
pub mod std_lib;

// Re-export the serve function and response helpers at crate root
pub use response::{html, json, not_found, text};
pub use routing::next_segment;
pub use server::serve;

pub mod prelude {
    pub use crate::bus::Bus;
    pub use crate::circuit::Circuit;
    pub use crate::module::{Module, ModuleError, ModuleResult};
    pub use crate::response::{html, json, not_found, text};
    pub use crate::routing::next_segment;
    pub use crate::serve;
    pub use crate::std_lib::debug::LogModule;
    pub use ranvier_macros::module;

    // Re-export common types users will need
    pub use bytes::Bytes;
    pub use http::{Method, Request, Response, StatusCode};
    pub use http_body_util::Full;
    // Re-export chrono for time safety if they need it
    pub use chrono::{DateTime, Utc};
}
