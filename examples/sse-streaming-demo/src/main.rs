//! # Server-Sent Events (SSE) Streaming Demo
//!
//! Demonstrates real-time event streaming using SSE with a custom EventSource implementation.
//!
//! ## Run
//! ```bash
//! cargo run -p sse-streaming-demo
//! ```
//!
//! ## Key Concepts
//! - Custom EventSource trait implementation for async event generation
//! - SSE stream conversion with `from_event_source`
//! - HTTP SSE endpoint integration with HttpIngress

use async_trait::async_trait;
use futures_core::stream::Stream;
use ranvier_core::event::EventSource;
use ranvier_core::prelude::*;
use ranvier_http::prelude::*;
use ranvier_runtime::Axon;
use std::convert::Infallible;
use std::pin::Pin;
use std::time::Duration;
use tokio::time::{interval, Interval};

struct TickerSource {
    ticker: Interval,
    count: usize,
}

impl TickerSource {
    fn new() -> Self {
        Self {
            ticker: interval(Duration::from_secs(1)),
            count: 0,
        }
    }
}

#[async_trait]
impl EventSource<String> for TickerSource {
    async fn next_event(&mut self) -> Option<String> {
        self.ticker.tick().await;
        self.count += 1;
        Some(format!("Tick {}", self.count))
    }
}

#[derive(Clone)]
struct SseHandler;

#[async_trait]
impl Transition<(), Sse<Pin<Box<dyn Stream<Item = Result<SseEvent, Infallible>> + Send + Sync>>>>
    for SseHandler
{
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        _s: (),
        _r: &(),
        _b: &mut Bus,
    ) -> Outcome<
        Sse<Pin<Box<dyn Stream<Item = Result<SseEvent, Infallible>> + Send + Sync>>>,
        Self::Error,
    > {
        let source = TickerSource::new();
        let stream =
            ranvier_http::sse::from_event_source(source, |msg| SseEvent::default().data(msg));
        Outcome::next(Sse::new(Box::pin(stream)))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("Starting Ranvier SSE Demo Server on http://127.0.0.1:3000/");
    println!("To test: curl -N http://127.0.0.1:3000/events");

    let handler = Axon::simple::<String>("sse").then(SseHandler);
    let app = HttpIngress::new().get("/events", handler);

    app.bind("127.0.0.1:3000")
        .run(())
        .await
        .map_err(|e| anyhow::anyhow!("{:?}", e))?;

    Ok(())
}
