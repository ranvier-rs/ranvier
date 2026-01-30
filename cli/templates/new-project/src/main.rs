//! {{project_name}}
//!
//! A Ranvier application.

use anyhow::Result;
use http::Request;
use ranvier_core::prelude::*;
use std::net::SocketAddr;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    let addr: SocketAddr = ([0, 0, 0, 0], 3000).into();

    tracing::info!("{{project_name}} starting on {}", addr);

    // TODO: Build your Axon here
    let axon = Axon::start((), "example")
        .then(HelloTransition);

    // Example: Execute with a request
    let req = Request::builder().uri("/").body(())?;
    let mut bus = Bus::new(req);

    match axon.execute(&mut bus).await? {
        Outcome::Next(result) => {
            println!("Result: {:?}", result);
        }
        Outcome::Fault(e) => {
            eprintln!("Error: {:?}", e);
        }
        _ => {}
    }

    Ok(())
}

// ============================================================
// Transitions
// ============================================================

pub struct HelloTransition;

impl Transition<(), String> for HelloTransition {
    async fn run(_input: (), _bus: &mut Bus) -> Outcome<String, anyhow::Error> {
        Outcome::Next("Hello from Ranvier!".to_string())
    }
}
