//! # ranvier-http - HTTP Ingress Adapter for Ranvier
//!
//! This crate provides the **Hyper 1.0 native boundary layer** for Ranvier.
//! It implements `Ranvier::http()` as an Ingress Circuit Builder (Discussion 193).
//!
//! ## Key Components
//!
//! - `Ranvier::http()` - Entry point for building HTTP ingress
//! - `HttpIngress` - Builder for configuring routes and server
//! - `RanvierService` - Hyper Service adapter for Axon execution
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

pub mod bus_ext;
pub mod extract;
pub mod guard_integration;
pub mod ingress;
pub mod response;
pub mod service;
pub mod sse;

#[cfg(feature = "htmx")]
pub mod htmx;
#[cfg(feature = "http3")]
pub mod http3;
pub mod test_harness;

pub use bus_ext::{BusHttpExt, json_outcome};
pub use extract::{CookieJar, DEFAULT_BODY_LIMIT, ExtractError, FromRequest, Header, Json, Path, Query};
pub use ingress::{
    HttpIngress, HttpRouteDescriptor, PathParams, QueryParams, Ranvier, WebSocketConnection,
    WebSocketError, WebSocketEvent, WebSocketSessionContext,
};
pub use response::{
    Html, HttpResponse, IntoProblemDetail, IntoResponse, ProblemDetail, json_error_response,
    outcome_to_problem_response, outcome_to_response, outcome_to_response_with_error,
};
pub use guard_integration::{
    BusInjectorFn, GuardExec, GuardIntegration, GuardRejection, PreflightConfig, RegisteredGuard,
    ResponseBodyTransformFn, ResponseExtractorFn,
};
pub use service::RanvierService;
pub use sse::{Sse, SseEvent};
pub use test_harness::{TestApp, TestHarnessError, TestRequest, TestResponse};

/// Collects Guard registrations for per-route Guard configuration.
///
/// Returns a `Vec<RegisteredGuard>` for use with `post_with_guards()`,
/// `get_with_guards()`, and other per-route Guard methods.
///
/// # Example
///
/// ```rust,ignore
/// use ranvier_http::guards;
/// use ranvier_guard::prelude::*;
///
/// Ranvier::http()
///     .guard(AccessLogGuard::new())  // global guard
///     .post_with_guards("/api/orders", order_circuit, guards![
///         ContentTypeGuard::json(),
///         IdempotencyGuard::ttl_5min(),
///     ])
///     .get("/api/orders", list_circuit)  // no extra guards
/// ```
#[macro_export]
macro_rules! guards {
    [$($guard:expr),* $(,)?] => {
        vec![$( $crate::GuardIntegration::register($guard) ),*]
    };
}

/// Prelude module for convenient imports
pub mod prelude {
    pub use crate::bus_ext::{BusHttpExt, json_outcome};
    pub use crate::extract::{CookieJar, DEFAULT_BODY_LIMIT, ExtractError, FromRequest, Header, Json, Path, Query};
    pub use crate::ingress::{
        HttpIngress, HttpRouteDescriptor, PathParams, QueryParams, Ranvier, WebSocketConnection,
        WebSocketError, WebSocketEvent, WebSocketSessionContext,
    };
    pub use crate::response::{
        Html, HttpResponse, IntoProblemDetail, IntoResponse, ProblemDetail, json_error_response,
        outcome_to_problem_response, outcome_to_response, outcome_to_response_with_error,
    };
    pub use crate::guard_integration::{
        BusInjectorFn, GuardExec, GuardIntegration, GuardRejection, PreflightConfig,
        RegisteredGuard, ResponseBodyTransformFn, ResponseExtractorFn,
    };
    pub use crate::service::RanvierService;
    pub use crate::sse::{Sse, SseEvent};
    pub use crate::test_harness::{TestApp, TestHarnessError, TestRequest, TestResponse};
}
