pub mod context;
pub mod pipeline;
pub mod response;
pub mod server;
#[path = "std/mod.rs"]
pub mod std_lib;
pub mod step;

// Re-export the serve function and response helpers at crate root
pub use response::{html, json, not_found, text};
pub use server::serve;

pub mod prelude {
    pub use crate::context::Context;
    pub use crate::pipeline::{Error, Pipeline, identity};
    pub use crate::response::{html, json, not_found, text};
    pub use crate::serve;
    pub use crate::std_lib::debug::LogLayer;

    // Re-export common types users will need
    pub use bytes::Bytes;
    pub use http::{Method, Request, Response, StatusCode};
    pub use http_body_util::Full;
}
