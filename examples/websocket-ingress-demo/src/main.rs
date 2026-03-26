//! # WebSocket Ingress
//!
//! Demonstrates WebSocket support with text/binary frame handling and session context.
//!
//! ## Run
//! ```bash
//! cargo run -p websocket-ingress-demo
//! ```
//!
//! ## Key Concepts
//! - WebSocket route handlers with `/ws/` prefix
//! - EventSource/EventSink traits for message streaming
//! - Session context access via Bus

use ranvier_core::event::{EventSink, EventSource};
use ranvier_http::prelude::*;
use serde::Serialize;

#[derive(Serialize)]
struct WelcomePayload {
    kind: &'static str,
    connection_id: String,
    path: String,
}

#[derive(Serialize)]
struct EchoPayload {
    kind: &'static str,
    message: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    println!("websocket-ingress-demo listening on ws://127.0.0.1:3130/ws/chat");
    println!("Send text frames and binary frames to see echo behavior.");

    Ranvier::http::<()>()
        .bind("127.0.0.1:3130")
        .ws("/ws/chat", |mut socket, _resources, bus| async move {
            if let Ok(session) = bus.get_cloned::<WebSocketSessionContext>() {
                let welcome = WelcomePayload {
                    kind: "welcome",
                    connection_id: session.connection_id().to_string(),
                    path: session.path().to_string(),
                };
                let _ = socket.send_json(&welcome).await;
            }

            while let Some(event) = socket.next_event().await {
                match event {
                    WebSocketEvent::Text(message) => {
                        let payload = EchoPayload {
                            kind: "echo",
                            message,
                        };
                        let _ = socket.send_json(&payload).await;
                    }
                    WebSocketEvent::Binary(bytes) => {
                        let _ = socket.send_event(bytes).await;
                    }
                    WebSocketEvent::Ping(bytes) => {
                        let _ = socket.send_event(WebSocketEvent::Pong(bytes)).await;
                    }
                    WebSocketEvent::Pong(_) => {}
                    WebSocketEvent::Close => break,
                }
            }
        })
        .run(())
        .await
}
