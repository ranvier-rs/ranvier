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

pub mod body;
pub mod extract;
pub mod ingress;
pub mod response;
pub mod sse;
pub mod service;
pub mod test_harness;

pub use body::{JsonBody, JsonBodyError};
pub use extract::{
    DEFAULT_BODY_LIMIT, ExtractError, FromRequest, HttpRequestBody, Json, Multipart, MultipartField,
    Path, Query,
};

pub use ingress::{
    HttpIngress, HttpRouteDescriptor, PathParams, Ranvier, RouteGroup, WebSocketConnection,
    WebSocketError, WebSocketEvent, WebSocketSessionContext,
};
pub use sse::{SseEvent, sse_response};
pub use response::{
    HttpResponse, IntoResponse, json_error_response, outcome_to_response,
    outcome_to_response_with_error,
};
pub use service::RanvierService;
pub use test_harness::{TestApp, TestHarnessError, TestRequest, TestResponse};

/// Prelude module for convenient imports
pub mod prelude {
    pub use crate::body::{JsonBody, JsonBodyError};
    pub use crate::extract::{
        DEFAULT_BODY_LIMIT, ExtractError, FromRequest, HttpRequestBody, Json, Multipart,
        MultipartField, Path, Query,
    };

    pub use crate::ingress::{
        HttpIngress, HttpRouteDescriptor, PathParams, Ranvier, RouteGroup, WebSocketConnection,
        WebSocketError, WebSocketEvent, WebSocketSessionContext,
    };
    pub use crate::sse::{SseEvent, sse_response};
    pub use crate::response::{
        HttpResponse, IntoResponse, json_error_response, outcome_to_response,
        outcome_to_response_with_error,
    };
    pub use crate::service::RanvierService;
    pub use crate::test_harness::{TestApp, TestHarnessError, TestRequest, TestResponse};
}
