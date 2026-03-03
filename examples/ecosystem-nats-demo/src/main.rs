use anyhow::{anyhow, Result};
use async_trait::async_trait;
use bytes::Bytes;
use futures_util::StreamExt;
use ranvier_core::event::{EventSink, EventSource};
use ranvier_core::prelude::*;
use ranvier_core::transition::ResourceRequirement;
use ranvier_runtime::Axon;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::time::{timeout, Duration};

#[derive(Clone)]
struct AppResources {
    nats: async_nats::Client,
    inbound_subject: String,
    outbound_subject: String,
}

impl ResourceRequirement for AppResources {}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

struct NatsSubjectSource {
    subscriber: async_nats::Subscriber,
}

#[async_trait]
impl EventSource<String> for NatsSubjectSource {
    async fn next_event(&mut self) -> Option<String> {
        self.subscriber
            .next()
            .await
            .and_then(|message| String::from_utf8(message.payload.to_vec()).ok())
    }
}

#[derive(Clone)]
struct NatsSubjectSink {
    client: async_nats::Client,
    subject: String,
}

#[async_trait]
impl EventSink<String> for NatsSubjectSink {
    type Error = Infallible;

    async fn send_event(&self, event: String) -> Result<(), Self::Error> {
        self.client
            .publish(self.subject.clone(), Bytes::from(event))
            .await
            .map_err(String::from)?;
        self.client.flush().await.map_err(String::from)?;
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct ParsedEvent {
    kind: String,
    payload: String,
}

#[derive(Clone, Copy)]
struct ParseEventTransition;

#[async_trait]
impl Transition<String, ParsedEvent> for ParseEventTransition {
    type Error = Infallible;
    type Resources = AppResources;

    async fn run(
        &self,
        input: String,
        resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<ParsedEvent, Self::Error> {
        let mut parts = input.splitn(2, ':');
        let kind = parts.next().unwrap_or("unknown").trim().to_string();
        let payload = parts.next().unwrap_or("").trim().to_string();
        bus.insert(format!(
            "route {} -> {}",
            resources.inbound_subject, resources.outbound_subject
        ));
        Outcome::Next(ParsedEvent { kind, payload })
    }
}

#[derive(Clone, Copy)]
struct ProjectToSinkTransition;

#[async_trait]
impl Transition<ParsedEvent, String> for ProjectToSinkTransition {
    type Error = Infallible;
    type Resources = AppResources;

    async fn run(
        &self,
        input: ParsedEvent,
        resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        let _client = resources.nats.clone();
        let route = bus
            .read::<String>()
            .cloned()
            .unwrap_or_else(|| "route unknown".to_string());
        let projection = format!(
            "projection kind={} payload={} via={} target={}",
            input.kind.to_uppercase(),
            input.payload,
            route,
            resources.outbound_subject
        );
        Outcome::Next(projection)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== M132 NATS Reference Demo ===");

    let nats_url = std::env::var("RANVIER_ECOSYSTEM_NATS_URL")
        .unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string());
    let client = match async_nats::connect(nats_url.clone()).await {
        Ok(client) => client,
        Err(err) => {
            println!("nats unavailable (url={}): {}", nats_url, err);
            println!("skip live pub/sub flow");
            return Ok(());
        }
    };

    let prefix = format!("ranvier.m132.nats.{}", now_ms());
    let inbound_subject = format!("{prefix}.inbound");
    let outbound_subject = format!("{prefix}.outbound");

    let inbound_subscriber = client.subscribe(inbound_subject.clone()).await?;
    let mut outbound_observer = client.subscribe(outbound_subject.clone()).await?;

    let resources = AppResources {
        nats: client.clone(),
        inbound_subject: inbound_subject.clone(),
        outbound_subject: outbound_subject.clone(),
    };

    let mut source = NatsSubjectSource {
        subscriber: inbound_subscriber,
    };
    let sink = NatsSubjectSink {
        client: client.clone(),
        subject: outbound_subject.clone(),
    };

    let axon = Axon::<String, String, String, AppResources>::new("nats.pub_sub_pipeline")
        .then(ParseEventTransition)
        .then(ProjectToSinkTransition);

    let inputs = vec![
        "order.created:id=o_100".to_string(),
        "order.completed:id=o_100".to_string(),
    ];

    for input in &inputs {
        client
            .publish(inbound_subject.clone(), Bytes::from(input.clone()))
            .await?;
    }
    client.flush().await?;

    for _ in 0..inputs.len() {
        let maybe_event = timeout(Duration::from_secs(3), source.next_event())
            .await
            .map_err(|_| anyhow!("timeout waiting for inbound NATS event"))?;
        let event = maybe_event.ok_or_else(|| anyhow!("inbound subject closed"))?;

        let mut bus = Bus::new();
        match axon.execute(event, &resources, &mut bus).await {
            Outcome::Next(projected) => {
                sink.send_event(projected).await?;
            }
            other => {
                return Err(anyhow!("axon execution failed: {:?}", other));
            }
        }
    }

    let mut received = Vec::new();
    for _ in 0..inputs.len() {
        let message = timeout(Duration::from_secs(3), outbound_observer.next())
            .await
            .map_err(|_| anyhow!("timeout waiting for outbound NATS event"))?
            .ok_or_else(|| anyhow!("outbound subject closed"))?;
        let payload = String::from_utf8(message.payload.to_vec())?;
        received.push(payload);
    }

    println!("published_to={}", inbound_subject);
    println!("received_from={}", outbound_subject);
    for event in received {
        println!("outbound={}", event);
    }
    println!("done");

    Ok(())
}
