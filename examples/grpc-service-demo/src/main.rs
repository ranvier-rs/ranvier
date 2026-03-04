//! # gRPC Service Demo
//!
//! Demonstrates the ranvier-grpc API surface including error mapping, response conversion, and metadata context extraction.
//!
//! ## Run
//! ```bash
//! cargo run -p grpc-service-demo
//! ```
//!
//! ## Key Concepts
//! - GrpcError types and tonic::Status conversion
//! - IntoGrpcResponse trait for Result types
//! - GrpcContext for metadata extraction (authorization, request-id)

use ranvier_grpc::error::GrpcError;
use ranvier_grpc::extract::GrpcContext;
use ranvier_grpc::response::IntoGrpcResponse;

fn main() {
    println!("=== Ranvier gRPC Service Demo ===");
    println!();
    println!("This demo illustrates the ranvier-grpc crate's API surface:");
    println!();

    // 1. Error mapping demonstration
    println!("1. Error Mapping:");
    let errors = vec![
        GrpcError::NotFound("user 42 not found".into()),
        GrpcError::InvalidArgument("name cannot be empty".into()),
        GrpcError::Unauthenticated("missing bearer token".into()),
        GrpcError::PermissionDenied("admin role required".into()),
    ];
    for err in errors {
        let status: tonic::Status = err.into();
        println!(
            "   Code={:?}, Message=\"{}\"",
            status.code(),
            status.message()
        );
    }
    println!();

    // 2. Response conversion demonstration
    println!("2. Response Conversion:");
    let ok_result: Result<String, GrpcError> = Ok("user created".into());
    match ok_result.into_grpc_response() {
        Ok(resp) => println!("   Ok response: {:?}", resp.get_ref()),
        Err(s) => println!("   Error status: {:?}", s),
    }

    let err_result: Result<String, GrpcError> = Err(GrpcError::NotFound("not found".into()));
    match err_result.into_grpc_response() {
        Ok(resp) => println!("   Ok response: {:?}", resp.get_ref()),
        Err(s) => println!(
            "   Error status: code={:?}, msg=\"{}\"",
            s.code(),
            s.message()
        ),
    }
    println!();

    // 3. GrpcIngress builder demonstration (API surface only)
    println!("3. GrpcIngress Builder API:");
    println!("   GrpcIngress::new()");
    println!("     .bind(\"0.0.0.0:50051\")");
    println!("     .add_service(my_service_server)");
    println!("     .run().await");
    println!();

    // 4. Metadata context demonstration
    println!("4. Metadata Context:");
    let mut metadata = tonic::metadata::MetadataMap::new();
    metadata.insert("authorization", "Bearer tok_demo_123".parse().unwrap());
    metadata.insert("x-request-id", "req-abc-456".parse().unwrap());

    let mut request = tonic::Request::new(());
    *request.metadata_mut() = metadata;

    let ctx = GrpcContext::from_request(&request);
    println!("   authorization = {:?}", ctx.authorization());
    println!("   x-request-id  = {:?}", ctx.get("x-request-id"));
    println!();

    println!("Demo complete. In production, use tonic-build to generate");
    println!("service stubs from .proto files and wire them through GrpcIngress.");
}
