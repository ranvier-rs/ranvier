/*!
# WebSocket Loop Example (Event Engine)

This example demonstrates the "Loop-Axon-Sink" pattern which is the foundation of Ranvier's Event Engine.

## Key Concepts

1.  **Event Source**: Something that produces events (e.g., WebSocket connection).
2.  **Event Sink**: Something that consumes events (e.g., WebSocket connection, Log).
3.  **Event Loop**: A loop that continuously pulls from Source, executes Axon, and pushes to Sink.
4.  **ConnectionBus**: A specialized Bus or pattern to hold connection-specific resources.

## Pattern

```text
[ EventSource ] --(Event)--> [ Axon ] --(Outcome)--> [ EventSink ]
       ^                          |
       |                          |
       +------(Loop continues)----+
```

*/

use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_core::event::{EventSource, EventSink};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

// ============================================================================
// 1. Data Types
// ============================================================================

#[derive(Debug, Clone)]
struct WsMessage {
    id: u64,
    content: String,
}

#[derive(Debug, Clone)]
struct ChatEvent {
    user_id: String,
    message: String,
}

// ============================================================================
// 2. Mock WebSocket Implementation
// ============================================================================

/// A Mock WebSocket that acts as both Source and Sink.
struct MockWebSocket {
    // Incoming messages from "client"
    incoming: VecDeque<WsMessage>,
    // Outgoing messages to "client"
    outgoing: Arc<Mutex<Vec<String>>>,
}

impl MockWebSocket {
    fn new(messages: Vec<&str>) -> Self {
        let incoming = messages
            .into_iter()
            .enumerate()
            .map(|(i, s)| WsMessage {
                id: i as u64,
                content: s.to_string(),
            })
            .collect();
        
        Self {
            incoming,
            outgoing: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl EventSource<WsMessage> for MockWebSocket {
    async fn next_event(&mut self) -> Option<WsMessage> {
        // Simulate network delay
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        self.incoming.pop_front()
    }
}

#[async_trait]
impl EventSink<String> for MockWebSocket {
    type Error = anyhow::Error;

    async fn send_event(&self, event: String) -> Result<(), Self::Error> {
        println!("[Sink] Sending to client: {}", event);
        self.outgoing.lock().unwrap().push(event);
        Ok(())
    }
}

// ============================================================================
// 3. Transitions
// ============================================================================

/// Transition: Process Message -> ChatEvent
#[derive(Clone)]
struct ProcessMessage;

#[async_trait]
impl Transition<WsMessage, ChatEvent> for ProcessMessage {
    type Error = anyhow::Error;

    async fn run(&self, input: WsMessage, _bus: &mut Bus) -> anyhow::Result<Outcome<ChatEvent, Self::Error>> {
        println!("[Process] Processing msg #{}: {}", input.id, input.content);
        
        // Simple logic: If message is "exit", stop the loop (in real app, we might Emit a Close event)
        if input.content == "exit" {
            // Check discussion 162: Control Flow. We can use Jump to End or similar.
            // For this demo, let's just process it.
        }

        Ok(Outcome::Next(ChatEvent {
            user_id: "user_1".to_string(),
            message: input.content.to_uppercase(),
        }))
    }
}

/// Transition: Broadcast (Simulated) -> String Response
#[derive(Clone)]
struct Broadcast;

#[async_trait]
impl Transition<ChatEvent, String> for Broadcast {
    type Error = anyhow::Error;

    async fn run(&self, input: ChatEvent, _bus: &mut Bus) -> anyhow::Result<Outcome<String, Self::Error>> {
        let response = format!("User {} said: {}", input.user_id, input.message);
        Ok(Outcome::Next(response))
    }
}

// ============================================================================
// 4. Main Loop
// ============================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== WebSocket Loop Demo (Event Engine) ===\n");

    // 1. Setup Mock Connection (Source + Sink)
    // In a real app, this would be a real WebSocket stream split into SplitStream/SplitSink
    let mut ws = MockWebSocket::new(vec![
        "hello",
        "ranvier",
        "is logic",
        "exit",
    ]);

    // We must clone the sink part to pass into the loop context if needed, 
    // or just use it at the end of the loop.
    // For traits, we usually wrap Sink in Arc<Mutex<>> or use channels if it requires mutability.
    // Here MockWebSocket implements EventSink via &self (interior mutability for outgoing), so reference is fine if lifetime allows.
    // But since loop runs async, we need shared ownership.
    let ws_sink = Arc::new(ws.outgoing.clone()); 
    // Note: In real Rust async, splitting Source/Sink is common. 
    // For this mock, `ws` holds state. We'll just run the loop on `ws`.
    
    // 2. The Event Loop
    println!("--- Starting Event Loop ---");
    while let Some(msg) = ws.next_event().await {
        println!("\n[Loop] Received event: {:?}", msg);

        // 3. Define the Axon for this event kind
        // Note: Axons are light and created per event typically, or reused if stateless.
        let axon = Axon::start(msg, "ChatFlow")
            .then(ProcessMessage)
            .then(Broadcast);

        let mut bus = Bus::new(http::Request::new(()));
        
        // 4. Execute Axon
        match axon.execute(&mut bus).await {
            Ok(Outcome::Next(response)) => {
                // 5. Send result to Sink
                if let Err(e) = ws.send_event(response).await {
                    eprintln!("Failed to send: {}", e);
                }
            }
            Ok(Outcome::Branch(_id, _val)) => {
                println!("Branched (not handled in this loop demo)");
            }
            Ok(_) => {}
            Err(e) => eprintln!("Axon Error: {}", e),
        }
    }

    println!("\n--- Loop Ended ---");
    let sent = ws_sink.lock().unwrap();
    println!("Total messages sent: {}", sent.len());

    Ok(())
}
