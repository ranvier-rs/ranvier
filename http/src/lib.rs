//! # ranvier-http - HTTP Ingress Adapter for Ranvier
//!
//! This crate provides the **Tower-native boundary layer** for Ranvier.
//! It implements `Ranvier::http()` as an Ingress Circuit Builder (Discussion 193).
//!
//! ## Key Components
//!
//! - `Ranvier::http()` - Entry point for building HTTP ingress
//! - `HttpIngress` - Builder for configuring routes and server
//! - `RanvierService` - Tower Service adapter for Axon execution
//!
//! ## Example
//!
//! ```rust,ignore
//! use ranvier_core::prelude::*;
//! use ranvier_http::prelude::*;
//!
//! let hello = Axon::new("Hello")
//!     .then(|_| async { "Hello, Ranvier!" });
//!
//! Ranvier::http()
//!     .bind("127.0.0.1:3000")
//!     .route("/", hello)
//!     .run()
//!     .await?;
//! ```

pub mod extract;
pub mod ingress;
pub mod response;
pub mod service;

pub use extract::{DEFAULT_BODY_LIMIT, ExtractError, FromRequest, Json, Path, Query};
pub use ingress::{HttpIngress, PathParams, Ranvier};
pub use response::{HttpResponse, IntoResponse, outcome_to_response};
pub use service::RanvierService;

/// Prelude module for convenient imports
pub mod prelude {
    pub use crate::extract::{DEFAULT_BODY_LIMIT, ExtractError, FromRequest, Json, Path, Query};
    pub use crate::ingress::{HttpIngress, PathParams, Ranvier};
    pub use crate::response::{HttpResponse, IntoResponse, outcome_to_response};
    pub use crate::service::RanvierService;
}
