use ranvier_core::event::EventSource;
use ranvier_http::prelude::*;
use async_trait::async_trait;
use std::convert::Infallible;
use futures_util::StreamExt;
use http_body_util::BodyExt;

struct MockSource {
    items: Vec<String>,
}

impl MockSource {
    fn new(items: Vec<&str>) -> Self {
        Self {
            items: items.into_iter().map(String::from).rev().collect(),
        }
    }
}

#[async_trait]
impl EventSource<String> for MockSource {
    async fn next_event(&mut self) -> Option<String> {
        self.items.pop()
    }
}

#[tokio::test]
async fn test_sse_into_response() {
    let source = MockSource::new(vec!["one", "two"]);
    
    let stream = ranvier_http::sse::from_event_source(source, |msg| {
        SseEvent::default().data(msg)
    });
    let stream = Box::pin(stream);
    
    let sse = Sse::new(stream);
    let response = sse.into_response();
    
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "text/event-stream"
    );

    let mut body = response.into_body();
    let frame1 = http_body_util::BodyExt::frame(&mut body).await.unwrap().unwrap();
    let data1: bytes::Bytes = frame1.into_data().unwrap();
    assert_eq!(String::from_utf8_lossy(&data1), "data: one\n\n");

    let frame2 = http_body_util::BodyExt::frame(&mut body).await.unwrap().unwrap();
    let data2: bytes::Bytes = frame2.into_data().unwrap();
    assert_eq!(String::from_utf8_lossy(&data2), "data: two\n\n");
}
