mod synapses;

use crate::synapses::HttpListenerSynapse;
use anyhow::Result;
use ranvier_core::prelude::*;
use ranvier_core::static_gen::StaticNode;
use ranvier_core::synapse::Synapse;
use std::sync::Arc;
use tiny_http::{Method, Response};

// --- Node: ProcessData (Matches basic-schematic) ---
// This guarantees the generated client's `useProcessData` hook works.
struct ProcessDataNode {
    synapse: Arc<HttpListenerSynapse>,
}

impl StaticNode for ProcessDataNode {
    fn id(&self) -> &'static str {
        "process_data"
    }
    fn kind(&self) -> NodeKind {
        NodeKind::Ingress
    }
    fn next_nodes(&self) -> Vec<&'static str> {
        vec![]
    }
}

impl ProcessDataNode {
    async fn execute(&self) -> Result<()> {
        // We look for GET /api/process_data
        match self.synapse.call(()).await {
            Ok(req) => {
                if req.method.as_str() == "GET" && req.url.contains("/api/process_data") {
                    println!("\x1b[32m[Node]\x1b[0m Received API Request: {}", req.url);

                    // Respond with JSON matching the expected output type (String)
                    // In basic-schematic: ProcessData: String -> String.
                    // The client expects the Ouptut of ProcessData which is String.
                    // For a query hook, it expects the JSON response.

                    let response_json = serde_json::json!("Processed Data from Backend");
                    let response = Response::from_string(response_json.to_string())
                        .with_header(
                            tiny_http::Header::from_bytes(
                                &b"Content-Type"[..],
                                &b"application/json"[..],
                            )
                            .unwrap(),
                        )
                        .with_header(
                            tiny_http::Header::from_bytes(
                                &b"Access-Control-Allow-Origin"[..],
                                &b"*"[..],
                            )
                            .unwrap(),
                        );

                    req.request.respond(response)?;
                } else {
                    // Ignore other requests or 404
                    let _ = req.request.respond(Response::empty(404));
                }
            }
            Err(_) => {}
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("\n=== Ranvier Full-Stack Backend (Port 3030) ===\n");

    let listener = Arc::new(HttpListenerSynapse::new(3030));

    let process_data = ProcessDataNode {
        synapse: listener.clone(),
    };

    // Main Loop
    loop {
        // Handle requests
        if let Err(e) = process_data.execute().await {
            eprintln!("Error: {}", e);
        }
    }
}
