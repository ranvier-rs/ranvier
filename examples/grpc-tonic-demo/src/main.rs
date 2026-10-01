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
use ranvier_runtime::{Axon, ExecutionReport};
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

fn validate_name(name: &str) -> Result<(), Status> {
    if name.trim().is_empty() || name.len() > 1024 {
        return Err(Status::invalid_argument(
            "Name must contain 1 to 1024 bytes",
        ));
    }
    Ok(())
}

#[cfg(test)]
include!("web_catalog_fixture.rs");

#[cfg(test)]
mod contract_tests {
    use super::*;
    use ranvier_runtime::{
        CompletionConfirmation as C, EffectBoundary, PersistenceOperation,
        SagaRollbackOutcome as O, SagaRollbackReport, SagaRollbackStep,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };
    use tokio_stream::{StreamExt, wrappers::TcpListenerStream};

    #[derive(Clone)]
    struct FailingGreeting(Arc<AtomicU64>);
    #[async_trait]
    impl Transition<GreetInput, GreetOutput> for FailingGreeting {
        type Error = String;
        type Resources = ();
        async fn run(&self, _: GreetInput, _: &(), _: &mut Bus) -> Outcome<GreetOutput, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Outcome::Fault("private-resolver-sentinel".into())
        }
    }

    #[test]
    fn report_policy_preserves_safe_statuses_and_cleanup() {
        let next = || {
            Outcome::Next(GreetOutput {
                message: "public".into(),
            })
        };
        assert_eq!(
            map_local_report(ExecutionReport::Unpersisted {
                result: next(),
                cleanup: None
            })
            .unwrap()
            .message,
            "public"
        );
        let fault = map_local_report(ExecutionReport::Unpersisted {
            result: Outcome::Fault("private".into()),
            cleanup: None,
        })
        .unwrap_err();
        assert_eq!(fault.code(), tonic::Code::Internal);
        assert_eq!(fault.message(), "Greeting failed");
        for report in [
            ExecutionReport::NotRequested {
                result: next(),
                cleanup: None,
            },
            ExecutionReport::Recorded {
                result: next(),
                completion: C::Confirmed,
                cleanup: None,
            },
            ExecutionReport::Recorded {
                result: next(),
                completion: C::NotRequested,
                cleanup: None,
            },
            ExecutionReport::Unconfirmed {
                result: next(),
                operation: PersistenceOperation::Result,
                effect: EffectBoundary::MayHaveRun,
                cleanup: None,
            },
            ExecutionReport::BlockedBeforeEffect {
                operation: PersistenceOperation::Intent,
                effect: EffectBoundary::NotInvoked,
                cleanup: None,
            },
        ] {
            let status = map_local_report(report).unwrap_err();
            assert_eq!(status.code(), tonic::Code::FailedPrecondition);
            assert_eq!(
                status.message(),
                "Execution requires reconciliation before retry"
            );
        }
        for finished in [false, true] {
            for outcome in [
                O::InProgress,
                O::Panicked,
                O::Completed,
                O::Fault,
                O::Emitted,
                O::Branched,
                O::Jumped,
                O::MissingHandler,
            ] {
                let result = map_local_report(ExecutionReport::Unpersisted {
                    result: next(),
                    cleanup: Some(SagaRollbackReport {
                        trace_id: "private".into(),
                        finished,
                        steps: vec![SagaRollbackStep {
                            node_id: "private".into(),
                            outcome,
                        }],
                    }),
                });
                assert_eq!(result.is_ok(), finished && outcome == O::Completed);
            }
        }
    }

    #[tokio::test]
    async fn failed_stream_stops_after_one_effect() {
        let calls = Arc::new(AtomicU64::new(0));
        let service = GreeterService {
            axon: Axon::<GreetInput, GreetInput, String>::new("failure")
                .then(FailingGreeting(calls.clone())),
        };
        let mut stream = service
            .say_hello_stream(Request::new(HelloRequest {
                name: "Synthetic".into(),
            }))
            .await
            .unwrap()
            .into_inner();
        let status = stream.next().await.unwrap().unwrap_err();
        assert_eq!(status.message(), "Greeting failed");
        assert!(stream.next().await.is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn real_tcp_unary_stream_and_invalid_input() {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let service = GreeterService {
                axon: Axon::<GreetInput, GreetInput, String>::new("tcp-greeting")
                    .then(BuildGreeting),
            };
            let (stop, stopped) = tokio::sync::oneshot::channel();
            let server = tokio::spawn(
                tonic::transport::Server::builder()
                    .add_service(GreeterServer::new(service))
                    .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                        let _ = stopped.await;
                    }),
            );
            let mut client =
                greeter::greeter_client::GreeterClient::connect(format!("http://{address}"))
                    .await
                    .unwrap();
            let reply = client
                .say_hello(HelloRequest {
                    name: "World".into(),
                })
                .await
                .unwrap()
                .into_inner();
            assert_eq!(reply.message, "Hello, World! (from Ranvier Axon pipeline)");
            for name in [" ".into(), "x".repeat(1025)] {
                let status = client
                    .say_hello(HelloRequest { name: name.clone() })
                    .await
                    .unwrap_err();
                assert_eq!(status.code(), tonic::Code::InvalidArgument);
                let status = client
                    .say_hello_stream(HelloRequest { name })
                    .await
                    .unwrap_err();
                assert_eq!(status.code(), tonic::Code::InvalidArgument);
            }
            let mut stream = client
                .say_hello_stream(HelloRequest {
                    name: "World".into(),
                })
                .await
                .unwrap()
                .into_inner();
            for index in 1..=3 {
                let reply = stream.message().await.unwrap().unwrap();
                assert_eq!(
                    reply.message,
                    format!("Hello, World (stream #{index})! (from Ranvier Axon pipeline)")
                );
            }
            assert!(stream.message().await.unwrap().is_none());
            drop(stream);
            drop(client);
            stop.send(()).unwrap();
            server.await.unwrap().unwrap();
        })
        .await
        .unwrap();
    }
}

// Local demo policy: no workflow persistence is configured.
fn map_local_report(
    report: ExecutionReport<Outcome<GreetOutput, String>>,
) -> Result<HelloReply, Status> {
    if report
        .cleanup()
        .is_some_and(|cleanup| !cleanup.is_complete())
    {
        return Err(Status::failed_precondition(
            "Execution requires reconciliation before retry",
        ));
    }
    match report {
        ExecutionReport::Unpersisted {
            result: Outcome::Next(output),
            ..
        } => Ok(HelloReply {
            message: output.message,
        }),
        ExecutionReport::Unpersisted {
            result: Outcome::Fault(_),
            ..
        } => Err(Status::internal("Greeting failed")),
        _ => Err(Status::failed_precondition(
            "Execution requires reconciliation before retry",
        )),
    }
}

async fn handle_hello(
    axon: &Axon<GreetInput, GreetOutput, String>,
    request: Request<HelloRequest>,
) -> Result<Response<HelloReply>, Status> {
    let name = request.into_inner().name;
    validate_name(&name)?;
    let mut bus = Bus::new();
    let report = axon.execute(GreetInput { name }, &(), &mut bus).await;
    map_local_report(report).map(Response::new)
}

#[tonic::async_trait]
impl Greeter for GreeterService {
    async fn say_hello(
        &self,
        request: Request<HelloRequest>,
    ) -> Result<Response<HelloReply>, Status> {
        handle_hello(&self.axon, request).await
    }

    type SayHelloStreamStream = tokio_stream::wrappers::ReceiverStream<Result<HelloReply, Status>>;

    async fn say_hello_stream(
        &self,
        request: Request<HelloRequest>,
    ) -> Result<Response<Self::SayHelloStreamStream>, Status> {
        let name = request.into_inner().name;
        validate_name(&name)?;
        let axon = self.axon.clone();
        let (tx, rx) = tokio::sync::mpsc::channel(4);

        tokio::spawn(async move {
            for i in 1..=3 {
                if tx.is_closed() {
                    break;
                }
                let mut bus = Bus::new();
                let input = GreetInput {
                    name: format!("{} (stream #{})", name, i),
                };
                // Observe an already-started effect before stopping on disconnect.
                let report = axon.execute(input, &(), &mut bus).await;
                let reply = map_local_report(report);
                let failed = reply.is_err();
                if tx.send(reply).await.is_err() {
                    break;
                }
                if failed {
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
        .unwrap_or_else(|_| "127.0.0.1:50051".into())
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
