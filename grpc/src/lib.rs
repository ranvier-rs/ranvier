//! # ranvier-grpc
//!
//! gRPC Ingress adapter for Ranvier. Bridges [tonic](https://docs.rs/tonic) gRPC
//! servers to Ranvier Axon circuits while maintaining the protocol-agnostic core
//! philosophy.
//!
//! ## Design Principle
//!
//! All gRPC-specific types live in this crate. `ranvier-core` has **zero**
//! dependency on `tonic` or `prost`. The boundary is explicit:
//!
//! ```text
//! gRPC client ──► tonic ──► GrpcIngress ──► Axon circuit ──► Bus/Transition
//! ```

pub mod error;
pub mod extract;
pub mod ingress;
pub mod response;
pub mod stream;
