use anyhow::Result;
use async_trait::async_trait;
use ranvier_core::synapse::Synapse;
use serde::Serialize;
use std::sync::Arc;
use tiny_http::{Response, Server};

// --- HTTP Listener Synapse ---
pub struct HttpListenerSynapse {
    pub server: Arc<Server>,
}

impl HttpListenerSynapse {
    pub fn new(port: u16) -> Self {
        let server = Server::http(format!("127.0.0.1:{}", port)).unwrap();
        println!(
            "\x1b[32m[HttpListener]\x1b[0m Listening on 127.0.0.1:{}",
            port
        );
        Self {
            server: Arc::new(server),
        }
    }
}

// Custom Input/Output types for this synapse
pub struct HttpRequest {
    pub url: String,
    pub method: String,
}

#[derive(Serialize)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

#[async_trait]
impl Synapse for HttpListenerSynapse {
    type Input = (); // Just wait for any request
    type Output = HttpRequest; // Return the request details
    type Error = String;

    async fn call(&self, _: Self::Input) -> Result<Self::Output, Self::Error> {
        // Blocks until a request is received (Pseudo-async for demo)
        // In a real async impl, we'd use tokio::net::TcpListener
        // tiny_http is synchronous, so we're blocking a thread here.
        println!("\x1b[36m[HttpListener]\x1b[0m Waiting for request...");

        let server = self.server.clone();

        // Wrap blocking call in spawn_blocking
        let request = tokio::task::spawn_blocking(move || server.recv())
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;

        // We can't return the Request object directly because of lifetimes/ownership in tiny_http
        // So we extract what we need and ideally we'd pass a channel to respond?
        // Synapse trait is Request-Response.
        // But HTTP Server is "Event Source".
        // This mismatch highlights why we usually use Synapse for OUTBOUND calls (DB, API).
        // For INBOUND (Triggers), we usually use the "Node" as the entry point calling `recv`.

        let url = request.url().to_string();
        let method = request.method().to_string();

        // For this demo, we immediately respond "200 OK" to acknowledge receipt,
        // OR we store the request in a buffer to be handled?
        // To keep it simple: We just return the metadata.
        // The demo logic assumes the Node *waits* for a request.

        // Quick hack: respond OK immediately so browser doesn't hang,
        // but in real world we might want to return the Workflow result.
        // Doing that with this Synapse trait structure (Call -> Return) is tricky for typical "Server" pattern.
        // BUT, we can make the Synapse `call` execute the generic "Wait for Request" action.

        // Respond with CORS for basic localhost dev
        let response = Response::from_string("{\"status\":\"processing\"}").with_header(
            tiny_http::Header::from_bytes(&b"Access-Control-Allow-Origin"[..], &b"*"[..]).unwrap(),
        );

        let _ = request.respond(response);

        Ok(HttpRequest { url, method })
    }
}
