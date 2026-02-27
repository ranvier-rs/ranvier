use ranvier_core::event::EventSource;
use ranvier_http::prelude::*;
use std::time::Duration;
use tokio::time::{interval, Interval};
use async_trait::async_trait;
use std::convert::Infallible;

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

async fn sse_handler() -> Result<Sse<impl futures_core::stream::Stream<Item = Result<SseEvent, Infallible>>>, Infallible> {
    let source = TickerSource::new();
    let stream = ranvier_http::sse::from_event_source(source, |msg| {
        SseEvent::default().data(msg)
    });
    Ok(Sse::new(stream))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("Starting Ranvier SSE Demo Server on http://127.0.0.1:3000/");
    println!("To test: curl -N http://127.0.0.1:3000/events");

    let app = HttpIngress::new()
        .route("/events", axum::routing::get(sse_handler));

    app.clone().bind("127.0.0.1:3000").run().await?;
    
    Ok(())
}
