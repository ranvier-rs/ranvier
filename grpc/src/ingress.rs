//! gRPC Ingress adapter for Ranvier.
//!
//! `GrpcIngress` is the primary builder for configuring and running a gRPC server
//! that bridges tonic services to Ranvier Axon circuits.
//!
//! # Example
//!
//! ```rust,ignore
//! use ranvier_grpc::ingress::GrpcIngress;
//!
//! GrpcIngress::new()
//!     .bind("[::1]:50051")
//!     .add_service(my_grpc_service)
//!     .run()
//!     .await?;
//! ```

use std::net::SocketAddr;
use tonic::transport::Server;
use tonic::transport::server::Router;

/// The primary builder for configuring a gRPC server backed by Ranvier Axon circuits.
///
/// Analogous to `HttpIngress` in `ranvier-http`, providing a declarative way to
/// configure gRPC endpoints while maintaining the protocol-agnostic core philosophy.
pub struct GrpcIngress {
    addr: SocketAddr,
    router: Option<Router>,
}

impl GrpcIngress {
    /// Create a new `GrpcIngress` instance with the default listen address `[::1]:50051`.
    pub fn new() -> Self {
        Self {
            addr: "[::1]:50051".parse().expect("default gRPC address"),
            router: None,
        }
    }

    /// Set the listen address for the gRPC server.
    ///
    /// # Examples
    /// ```rust,ignore
    /// GrpcIngress::new().bind("0.0.0.0:50051")
    /// ```
    pub fn bind(mut self, addr: &str) -> Self {
        self.addr = addr
            .parse()
            .unwrap_or_else(|_| panic!("invalid gRPC bind address: {addr}"));
        self
    }

    /// Add a tonic service to the gRPC server.
    ///
    /// This is the primary method for registering gRPC service implementations.
    /// Each service is a tonic-generated server type that implements `tonic::codegen::Service`.
    pub fn add_service<S>(mut self, service: S) -> Self
    where
        S: tonic::codegen::Service<
                http::Request<tonic::body::BoxBody>,
                Response = http::Response<tonic::body::BoxBody>,
                Error = std::convert::Infallible,
            > + tonic::server::NamedService
            + Clone
            + Send
            + 'static,
        S::Future: Send + 'static,
    {
        let router = match self.router.take() {
            Some(r) => r.add_service(service),
            None => Server::builder().add_service(service),
        };
        self.router = Some(router);
        self
    }

    /// Run the gRPC server, blocking until shutdown.
    ///
    /// Listens on the configured address and serves all registered services.
    pub async fn run(self) -> Result<(), tonic::transport::Error> {
        let router = self
            .router
            .expect("at least one gRPC service must be added");

        tracing::info!("gRPC server listening on {}", self.addr);

        router.serve(self.addr).await
    }

    /// Run the gRPC server with a graceful shutdown signal.
    pub async fn run_with_shutdown<F>(self, signal: F) -> Result<(), tonic::transport::Error>
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let router = self
            .router
            .expect("at least one gRPC service must be added");

        tracing::info!(
            "gRPC server listening on {} (graceful shutdown enabled)",
            self.addr
        );

        router.serve_with_shutdown(self.addr, signal).await
    }
}

impl Default for GrpcIngress {
    fn default() -> Self {
        Self::new()
    }
}
