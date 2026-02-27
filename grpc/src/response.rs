//! gRPC response conversion for Ranvier.
//!
//! Provides the `IntoGrpcResponse` trait to convert Axon circuit outputs
//! into tonic gRPC responses.

/// Trait for converting an Axon output into a tonic gRPC response.
///
/// Analogous to `IntoResponse` in `ranvier-http` but for gRPC unary RPCs.
pub trait IntoGrpcResponse<T> {
    fn into_grpc_response(self) -> Result<tonic::Response<T>, tonic::Status>;
}

/// Implementation for `Result<T, E>` where E converts to tonic::Status.
impl<T, E> IntoGrpcResponse<T> for Result<T, E>
where
    E: Into<tonic::Status>,
{
    fn into_grpc_response(self) -> Result<tonic::Response<T>, tonic::Status> {
        match self {
            Ok(val) => Ok(tonic::Response::new(val)),
            Err(e) => Err(e.into()),
        }
    }
}
