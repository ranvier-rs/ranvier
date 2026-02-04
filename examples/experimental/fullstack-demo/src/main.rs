mod synapses;

use crate::synapses::HttpListenerSynapse;
use anyhow::Result;
use ranvier_core::prelude::NodeKind;
use ranvier_core::static_gen::StaticNode;
use ranvier_core::synapse::Synapse;
use std::sync::Arc;
use tiny_http::Response;

// Experimental inbound HTTP node for local frontend wiring.
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
        // Accept both legacy GET and current POST route for easier experimental testing.
        match self.synapse.call(()).await {
            Ok(req) => {
                if req.method.as_str() == "OPTIONS" {
                    let response = with_cors_headers(Response::empty(204));
                    req.request.respond(response)?;
                    return Ok(());
                }

                let is_legacy = req.method.as_str() == "GET" && req.url.contains("/api/process_data");
                let is_order = req.method.as_str() == "POST" && req.url.contains("/api/order");

                if is_legacy || is_order {
                    println!("\x1b[32m[Node]\x1b[0m Received API Request: {}", req.url);

                    let response_json = if is_order {
                        serde_json::json!({
                            "status": "accepted",
                            "order_id": "ORDER-SUCCESS-999",
                            "message": "Order received by experimental backend"
                        })
                    } else {
                        serde_json::json!("Processed Data from Backend")
                    };
                    let response = with_cors_headers(
                        Response::from_string(response_json.to_string()).with_header(
                            tiny_http::Header::from_bytes(
                                &b"Content-Type"[..],
                                &b"application/json"[..],
                            )
                            .unwrap(),
                        ),
                    );

                    req.request.respond(response)?;
                } else {
                    let _ = req.request.respond(with_cors_headers(Response::empty(404)));
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

fn with_cors_headers<R: std::io::Read>(response: Response<R>) -> Response<R> {
    response
        .with_header(
            tiny_http::Header::from_bytes(&b"Access-Control-Allow-Origin"[..], &b"*"[..]).unwrap(),
        )
        .with_header(
            tiny_http::Header::from_bytes(&b"Access-Control-Allow-Methods"[..], &b"GET, POST, OPTIONS"[..]).unwrap(),
        )
        .with_header(
            tiny_http::Header::from_bytes(
                &b"Access-Control-Allow-Headers"[..],
                &b"Content-Type"[..],
            )
            .unwrap(),
        )
}
