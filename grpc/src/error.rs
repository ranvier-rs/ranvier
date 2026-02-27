//! gRPC error mapping for Ranvier.
//!
//! Maps Ranvier outcomes to gRPC status codes.

use tonic::{Code, Status};

/// Maps a debug-formatted error message to a gRPC `Status`.
///
/// This provides a default mapping from common error patterns to gRPC status codes.
/// Users can override this by implementing custom error handlers on `GrpcIngress`.
pub fn default_error_status<E: std::fmt::Debug>(error: E) -> Status {
    Status::new(Code::Internal, format!("{:?}", error))
}

/// Well-known mapping from semantic error categories to gRPC status codes.
///
/// Users can use this enum to express intent and produce correctly-typed gRPC errors.
#[derive(Debug, Clone)]
pub enum GrpcError {
    /// The request was invalid (Code::InvalidArgument).
    InvalidArgument(String),
    /// The requested resource was not found (Code::NotFound).
    NotFound(String),
    /// The caller does not have permission (Code::PermissionDenied).
    PermissionDenied(String),
    /// The caller is not authenticated (Code::Unauthenticated).
    Unauthenticated(String),
    /// The server encountered an internal error (Code::Internal).
    Internal(String),
    /// The operation is not implemented (Code::Unimplemented).
    Unimplemented(String),
    /// The service is unavailable (Code::Unavailable).
    Unavailable(String),
    /// The operation timed out (Code::DeadlineExceeded).
    DeadlineExceeded(String),
    /// The resource already exists (Code::AlreadyExists).
    AlreadyExists(String),
    /// A precondition for the operation was not met (Code::FailedPrecondition).
    FailedPrecondition(String),
}

impl From<GrpcError> for Status {
    fn from(err: GrpcError) -> Self {
        match err {
            GrpcError::InvalidArgument(msg) => Status::invalid_argument(msg),
            GrpcError::NotFound(msg) => Status::not_found(msg),
            GrpcError::PermissionDenied(msg) => Status::permission_denied(msg),
            GrpcError::Unauthenticated(msg) => Status::unauthenticated(msg),
            GrpcError::Internal(msg) => Status::internal(msg),
            GrpcError::Unimplemented(msg) => Status::unimplemented(msg),
            GrpcError::Unavailable(msg) => Status::unavailable(msg),
            GrpcError::DeadlineExceeded(msg) => Status::deadline_exceeded(msg),
            GrpcError::AlreadyExists(msg) => Status::already_exists(msg),
            GrpcError::FailedPrecondition(msg) => Status::failed_precondition(msg),
        }
    }
}
