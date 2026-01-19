pub mod context;
pub mod pipeline;
pub mod server;
#[path = "std/mod.rs"]
pub mod std_lib;
pub mod step;

// Re-export the serve function at crate root
pub use server::serve;

pub mod prelude {
    pub use crate::context::Context;
    pub use crate::pipeline::{Error, Pipeline, identity};
    pub use crate::serve;
    pub use crate::std_lib::debug::LogLayer;

    // Re-export common types users will need
    pub use bytes::Bytes;
    pub use http::{Request, Response};
    pub use http_body_util::Full;
}
