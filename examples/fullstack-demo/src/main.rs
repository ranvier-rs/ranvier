mod synapses;

use crate::synapses::HttpListenerSynapse;
use anyhow::Result;
use ranvier_core::prelude::*;
use ranvier_core::static_gen::StaticNode;
use ranvier_core::synapse::Synapse;
use std::sync::Arc;

// --- Node: WaitForRequest ---
// This node blocks until an HTTP request arrives.
struct WaitForRequestNode {
    synapse: Arc<HttpListenerSynapse>,
    next: &'static str,
}

impl StaticNode for WaitForRequestNode {
    fn id(&self) -> &'static str {
        "wait_for_request"
    }
    fn kind(&self) -> NodeKind {
        NodeKind::Ingress
    } // It's an entry point
    fn next_nodes(&self) -> Vec<&'static str> {
        vec![self.next]
    }
}

impl WaitForRequestNode {
    async fn execute(&self) -> Result<Outcome<String, String>> {
        println!("\x1b[1m[Node]\x1b[0m Waiting for HTTP Order...");

        match self.synapse.call(()).await {
            Ok(req) => {
                println!("\x1b[32m[Node]\x1b[0m Received {} {}", req.method, req.url);
                Ok(Outcome::Next(req.url)) // Pass URL as payload
            }
            Err(e) => Ok(Outcome::Fault(e)),
        }
    }
}

// --- Node: ProcessOrder ---
struct ProcessOrderNode {
    next: &'static str,
}

impl StaticNode for ProcessOrderNode {
    fn id(&self) -> &'static str {
        "process_order"
    }
    fn kind(&self) -> NodeKind {
        NodeKind::Atom
    }
    fn next_nodes(&self) -> Vec<&'static str> {
        vec![self.next]
    }
}

impl ProcessOrderNode {
    async fn execute(&self, url: String) -> Result<Outcome<String, String>> {
        println!("\x1b[1m[Node]\x1b[0m Processing Order from URL: {}", url);
        // Simulate logic
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        Ok(Outcome::Next("ORDER-SUCCESS-999".to_string()))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("\n=== Ranvier Full-Stack Backend (Port 3030) ===\n");

    let listener = Arc::new(HttpListenerSynapse::new(3030));

    let ingress = WaitForRequestNode {
        synapse: listener.clone(),
        next: "process_order",
    };

    let processor = ProcessOrderNode { next: "end" };

    // Main Loop: simple server loop
    loop {
        // Step 1: Ingress
        match ingress.execute().await? {
            Outcome::Next(payload) => {
                // Step 2: Process
                match processor.execute(payload).await? {
                    Outcome::Next(result) => {
                        println!("\x1b[1m[Node]\x1b[0m Finished: {}\n", result);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}
