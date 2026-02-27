//! gRPC request extraction for Ranvier.
//!
//! Provides the `FromGrpcRequest` trait to bridge tonic requests
//! into Ranvier-compatible types, and utilities for extracting
//! gRPC metadata into Bus context.

use async_trait::async_trait;
use std::collections::HashMap;
use tonic::metadata::MetadataMap;

/// Trait for extracting typed data from a tonic gRPC request.
///
/// Analogous to `ranvier_http::extract::FromRequest` but for gRPC.
#[async_trait]
pub trait FromGrpcRequest<T>: Sized {
    type Error: Into<tonic::Status>;
    async fn from_grpc_request(request: tonic::Request<T>) -> Result<Self, Self::Error>;
}

/// Extract key-value metadata from a gRPC `MetadataMap` into a `HashMap`.
///
/// Only ASCII metadata values are extracted; binary metadata is skipped.
pub fn extract_metadata(metadata: &MetadataMap) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for key_and_value in metadata.iter() {
        if let tonic::metadata::KeyAndValueRef::Ascii(key, value) = key_and_value {
            if let Ok(v) = value.to_str() {
                map.insert(key.as_str().to_string(), v.to_string());
            }
        }
    }
    map
}

/// gRPC metadata context that can be injected into the Ranvier Bus.
///
/// Holds the extracted metadata key-value pairs and common fields
/// like authority, remote address, etc.
#[derive(Debug, Clone)]
pub struct GrpcContext {
    /// Metadata key-value pairs extracted from the incoming gRPC request.
    pub metadata: HashMap<String, String>,
    /// The `:authority` pseudo-header, if present.
    pub authority: Option<String>,
    /// Remote address of the client, if available.
    pub remote_addr: Option<String>,
}

impl GrpcContext {
    /// Create a `GrpcContext` from a tonic request's metadata.
    pub fn from_request<T>(request: &tonic::Request<T>) -> Self {
        let metadata = extract_metadata(request.metadata());
        let authority = metadata.get("authority").cloned();
        let remote_addr = request.remote_addr().map(|a| a.to_string());

        Self {
            metadata,
            authority,
            remote_addr,
        }
    }

    /// Get a metadata value by key.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.metadata.get(key).map(|s| s.as_str())
    }

    /// Get the authorization header value (commonly used for auth bridging).
    pub fn authorization(&self) -> Option<&str> {
        self.get("authorization")
    }
}
