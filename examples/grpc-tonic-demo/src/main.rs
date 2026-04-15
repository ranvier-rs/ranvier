//! # gRPC Tonic Demo
//!
//! Demonstrates using `tonic` gRPC server alongside Ranvier Axon pipelines.
//! Replaces the removed `ranvier-grpc` wrapper crate.
//!
//! ## Run
//! ```bash
//! cargo run -p grpc-tonic-demo
//! ```
//!
//! ## Key Concepts
//! - Define gRPC services with `.proto` files and `tonic-build`
//! - Implement gRPC service handlers that delegate to Axon pipelines
//! - Run gRPC server and Ranvier in the same process
//! - No wrapper crate needed — `tonic` + `prost` + Axon is sufficient

use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_runtime::Axon;
use serde::{Deserialize, Serialize};
use tonic::{Request, Response, Status};

pub mod greeter {
    tonic::include_proto!("greeter");
}

use greeter::greeter_server::{Greeter, GreeterServer};
use greeter::{HelloReply, HelloRequest};

// ============================================================================
// Axon Pipeline for greeting logic
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
struct GreetInput {
    name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct GreetOutput {
    message: String,
}

#[derive(Clone)]
struct BuildGreeting;

#[async_trait]
impl Transition<GreetInput, GreetOutput> for BuildGreeting {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: GreetInput,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<GreetOutput, Self::Error> {
        Outcome::next(GreetOutput {
            message: format!("Hello, {}! (from Ranvier Axon pipeline)", input.name),
        })
    }
}

// ============================================================================
// gRPC Service Implementation
// ============================================================================

struct GreeterService {
    axon: Axon<GreetInput, GreetOutput, String>,
}

#[tonic::async_trait]
impl Greeter for GreeterService {
    async fn say_hello(
        &self,
        request: Request<HelloRequest>,
    ) -> Result<Response<HelloReply>, Status> {
        let name = request.into_inner().name;
        let mut bus = Bus::new();
        let result = self.axon.execute(GreetInput { name }, &(), &mut bus).await;

        match result {
            Outcome::Next(output) => Ok(Response::new(HelloReply {
                message: output.message,
            })),
            Outcome::Fault(e) => Err(Status::internal(e)),
            _ => Err(Status::internal("Unexpected outcome")),
        }
    }

    type SayHelloStreamStream = tokio_stream::wrappers::ReceiverStream<Result<HelloReply, Status>>;

    async fn say_hello_stream(
        &self,
        request: Request<HelloRequest>,
    ) -> Result<Response<Self::SayHelloStreamStream>, Status> {
        let name = request.into_inner().name;
        let axon = self.axon.clone();
        let (tx, rx) = tokio::sync::mpsc::channel(4);

        tokio::spawn(async move {
            for i in 1..=3 {
                let mut bus = Bus::new();
                let input = GreetInput {
                    name: format!("{} (stream #{})", name, i),
                };
                let result = axon.execute(input, &(), &mut bus).await;
                let reply = match result {
                    Outcome::Next(output) => Ok(HelloReply {
                        message: output.message,
                    }),
                    _ => Err(Status::internal("Pipeline error")),
                };
                if tx.send(reply).await.is_err() {
                    break;
                }
            }
        });

        Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(
            rx,
        )))
    }
}

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== gRPC Tonic Demo ===\n");

    let axon = Axon::<GreetInput, GreetInput, String>::new("grpc-greeter").then(BuildGreeting);

    let addr = std::env::var("GRPC_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:50051".into())
        .parse()
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    let service = GreeterService { axon };

    println!("gRPC server listening on {}", addr);
    println!(
        "  grpcurl -plaintext {} greeter.Greeter/SayHello -d '{{\"name\":\"World\"}}'",
        addr
    );

    tonic::transport::Server::builder()
        .add_service(GreeterServer::new(service))
        .serve(addr)
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    Ok(())
}
