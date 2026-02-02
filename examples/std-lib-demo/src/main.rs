use http::Request;
use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use std::convert::Infallible;
use std::net::SocketAddr;
use tokio::net::TcpListener;

use ranvier_core::prelude::*;
use ranvier_http::RanvierService;
use ranvier_std::prelude::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    println!("Running Ranvier Standard Library Demo (Hyper/Tower Foundation)...");

    // 1. Logic Demo Pipeline
    let filter = FilterNode::new(|s: &String| s.len() > 5);
    let switch = SwitchNode::new(|s: &String| {
        if s.contains("Hello") {
            "greeting".to_string()
        } else {
            "other".to_string()
        }
    });

    // Define Axon: In=String, Out=String
    // We explicitly specify types since start() doesn't take value to infer from
    let logic_pipeline = Axon::<String, String, Infallible>::start("Logic Demo")
        .then(LogNode::new("Start", "info"))
        .then(filter)
        .then(switch);

    // Create the Service
    // Converter: Request -> String ("Hello Ranvier")
    let converter =
        |req: Request<hyper::body::Incoming>, _bus: &mut Bus| "Hello Ranvier".to_string();

    let service = RanvierService::new(logic_pipeline, converter);

    // Bind to port
    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    let listener = TcpListener::bind(addr).await?;
    println!("Listening on http://{}", addr);

    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let service_clone = service.clone();
        let hyper_service = TowerToHyperService::new(service_clone);

        tokio::task::spawn(async move {
            if let Err(err) = http1::Builder::new()
                .serve_connection(io, hyper_service)
                .await
            {
                println!("Error serving connection: {:?}", err);
            }
        });
    }
}
