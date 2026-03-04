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
pub mod sse;

#[cfg(feature = "http3")]
pub mod http3;
pub mod test_harness;

pub use extract::{CookieJar, DEFAULT_BODY_LIMIT, ExtractError, FromRequest, Header, Json, Path, Query};
pub use ingress::{
    HttpIngress, HttpRouteDescriptor, PathParams, Ranvier, WebSocketConnection, WebSocketError,
    WebSocketEvent, WebSocketSessionContext,
};
pub use response::{
    Html, HttpResponse, IntoResponse, json_error_response, outcome_to_response,
    outcome_to_response_with_error,
};
pub use service::RanvierService;
pub use sse::{Sse, SseEvent};
pub use test_harness::{TestApp, TestHarnessError, TestRequest, TestResponse};

/// Prelude module for convenient imports
pub mod prelude {
    pub use crate::extract::{CookieJar, DEFAULT_BODY_LIMIT, ExtractError, FromRequest, Header, Json, Path, Query};
    pub use crate::ingress::{
        HttpIngress, HttpRouteDescriptor, PathParams, Ranvier, WebSocketConnection, WebSocketError,
        WebSocketEvent, WebSocketSessionContext,
    };
    pub use crate::response::{
        Html, HttpResponse, IntoResponse, json_error_response, outcome_to_response,
        outcome_to_response_with_error,
    };
    pub use crate::service::RanvierService;
    pub use crate::sse::{Sse, SseEvent};
    pub use crate::test_harness::{TestApp, TestHarnessError, TestRequest, TestResponse};
}
