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

pub mod ingress;
pub mod service;

pub use ingress::{HttpIngress, Ranvier};
pub use service::RanvierService;

/// Prelude module for convenient imports
pub mod prelude {
    pub use crate::ingress::{HttpIngress, Ranvier};
    pub use crate::service::RanvierService;
}
