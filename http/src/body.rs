//! # Body Ergonomics — Transition-Level Body Extraction
//!
//! Provides `JsonBody<T>` — a `Transition` that reads the raw HTTP request body
//! from the `Bus` (injected by `.post_body()` / `.put_body()` / `.patch_body()`)
//! and deserializes it as JSON.
//!
//! ## Usage
//!
//! ```rust,ignore
//! use ranvier_http::prelude::*;
//! use serde::Deserialize;
//!
//! #[derive(Deserialize)]
//! struct CreateNote { title: String, body: String }
//!
//! let create = Axon::new("CreateNote")
//!     .then(JsonBody::<CreateNote>::new())
//!     .then(|note: CreateNote| async move {
//!         format!("Created: {}", note.title)
//!     });
//!
//! Ranvier::http()
//!     .post_body("/notes", create)
//!     .run(resources)
//!     .await?;
//! ```

use std::marker::PhantomData;

use async_trait::async_trait;
use bytes::Bytes;
use http::Response;
use http::StatusCode;
use http_body_util::{BodyExt, Full};
use ranvier_core::prelude::*;
use ranvier_core::transition::ResourceRequirement;
use serde::de::DeserializeOwned;

use crate::extract::HttpRequestBody;
use crate::response::{IntoResponse, RanvierResponse};

/// A `Transition` that reads `HttpRequestBody` from the [`Bus`] and deserializes it as JSON.
///
/// Requires the route to be registered with `.post_body()`, `.put_body()`, or `.patch_body()`
/// so that the ingress collects and inserts the body bytes before the circuit runs.
///
/// # Type Parameters
///
/// - `T`: The deserialization target type. Must implement `serde::DeserializeOwned + Send + 'static`.
/// - `R`: The resource requirement type for the circuit.
pub struct JsonBody<T, R = ()> {
    _phantom: PhantomData<(T, R)>,
}

impl<T, R> JsonBody<T, R> {
    pub fn new() -> Self {
        Self {
            _phantom: PhantomData,
        }
    }
}

impl<T, R> Default for JsonBody<T, R> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, R> Clone for JsonBody<T, R> {
    fn clone(&self) -> Self {
        Self::new()
    }
}

impl<T, R> std::fmt::Debug for JsonBody<T, R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "JsonBody<{}>", std::any::type_name::<T>())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum JsonBodyError {
    #[error("missing HttpRequestBody in bus — use .post_body()/.put_body()/.patch_body() for this route")]
    MissingBody,
    #[error("failed to parse JSON body: {0}")]
    ParseError(String),
}

impl IntoResponse for JsonBodyError {
    fn into_response(self) -> RanvierResponse {
        let status = match self {
            Self::MissingBody => StatusCode::INTERNAL_SERVER_ERROR,
            Self::ParseError(_) => StatusCode::BAD_REQUEST,
        };
        Response::builder()
            .status(status)
            .body(Full::new(Bytes::from(self.to_string())).map_err(|e| match e {}).boxed())
            .unwrap()
    }
}

#[async_trait]
impl<T, R> Transition<(), T> for JsonBody<T, R>
where
    T: DeserializeOwned + Send + Sync + 'static,
    R: ResourceRequirement + Clone + Send + Sync + 'static,
{
    type Error = JsonBodyError;
    type Resources = R;

    async fn run(&self, _input: (), _res: &R, bus: &mut Bus) -> Outcome<T, JsonBodyError> {
        let bytes = match bus.read::<HttpRequestBody>() {
            Some(body) => body.0.clone(),
            None => return Outcome::Fault(JsonBodyError::MissingBody),
        };

        match serde_json::from_slice::<T>(&bytes) {
            Ok(value) => Outcome::Next(value),
            Err(e) => Outcome::Fault(JsonBodyError::ParseError(e.to_string())),
        }
    }
}
