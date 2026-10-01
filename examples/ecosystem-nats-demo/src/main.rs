//! # NATS Event Streaming
//!
//! Demonstrates pub/sub event routing with NATS using EventSource and EventSink.
//!
//! ## Run
//! ```bash
//! cargo run -p ecosystem-nats-demo
//! ```
//!
//! ## Key Concepts
//! - EventSource for consuming NATS subjects
//! - EventSink for publishing events
//! - Axon pipeline for event transformation

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use bytes::Bytes;
use futures_util::StreamExt;
use ranvier_core::event::{EventSink, EventSource};
use ranvier_core::prelude::*;
use ranvier_runtime::Axon;
use tokio::time::{Duration, timeout};

// Explicit local-demo policy: neither persisted nor incomplete cleanup results
// may be converted into ordinary domain success or automatic retry permission.
fn local_demo_outcome<T, E>(
    report: ranvier_runtime::ExecutionReport<Outcome<T, E>>,
) -> anyhow::Result<Outcome<T, E>> {
    if report
        .cleanup()
        .is_some_and(|cleanup| !cleanup.is_complete())
    {
        anyhow::bail!("Execution requires reconciliation before retry");
    }
    match report {
        ranvier_runtime::ExecutionReport::Unpersisted { result, .. } => Ok(result),
        _ => anyhow::bail!("Execution requires reconciliation before retry"),
    }
}

#[derive(Clone)]
struct AppResources {
    inbound_subject: String,
    outbound_subject: String,
}

const MAX_EVENT_BYTES: usize = 65536;
#[derive(Clone)]
struct RouteContext {
    inbound: String,
    outbound: String,
}

struct NatsSubjectSource {
    subscriber: async_nats::Subscriber,
}

#[async_trait]
impl EventSource<Result<String, String>> for NatsSubjectSource {
    async fn next_event(&mut self) -> Option<Result<String, String>> {
        self.subscriber.next().await.map(|message| {
            if message.payload.len() > MAX_EVENT_BYTES {
                return Err("Inbound event too large".into());
            }
            String::from_utf8(message.payload.to_vec()).map_err(|_| "Invalid inbound UTF-8".into())
        })
    }
}

#[derive(Clone)]
struct NatsSubjectSink {
    client: async_nats::Client,
    subject: String,
}

#[async_trait]
impl EventSink<String> for NatsSubjectSink {
    type Error = String;

    async fn send_event(&self, event: String) -> Result<(), Self::Error> {
        if event.len() > MAX_EVENT_BYTES {
            return Err("Outbound event too large".into());
        }
        self.client
            .publish(self.subject.clone(), Bytes::from(event))
            .await
            .map_err(|_| "Publication requires reconciliation".to_owned())?;
        self.client
            .flush()
            .await
            .map_err(|_| "Publication requires reconciliation".to_owned())?;
        Ok(())
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct ParsedEvent {
    kind: String,
    payload: String,
}

#[derive(Clone, Copy)]
struct ParseEventTransition;

#[async_trait]
impl Transition<String, ParsedEvent> for ParseEventTransition {
    type Error = String;
    type Resources = AppResources;

    async fn run(
        &self,
        input: String,
        resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<ParsedEvent, Self::Error> {
        if input.len() > MAX_EVENT_BYTES {
            return Outcome::Fault("Inbound event too large".into());
        }
        let Some((kind, payload)) = input.split_once(':') else {
            return Outcome::Fault("Invalid event format".into());
        };
        let kind = kind.trim().to_string();
        let payload = payload.trim().to_string();
        if kind.is_empty()
            || kind.len() > 128
            || !kind
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
            || payload.is_empty()
        {
            return Outcome::Fault("Invalid event format".into());
        }
        bus.insert(RouteContext {
            inbound: resources.inbound_subject.clone(),
            outbound: resources.outbound_subject.clone(),
        });
        Outcome::Next(ParsedEvent { kind, payload })
    }
}

#[derive(Clone, Copy)]
struct ProjectToSinkTransition;

#[async_trait]
impl Transition<ParsedEvent, String> for ProjectToSinkTransition {
    type Error = String;
    type Resources = AppResources;

    async fn run(
        &self,
        input: ParsedEvent,
        resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        let Some(route) = bus.read::<RouteContext>() else {
            return Outcome::Fault("Route context missing".into());
        };
        if route.inbound != resources.inbound_subject
            || route.outbound != resources.outbound_subject
        {
            return Outcome::Fault("Route context mismatch".into());
        }
        let route = format!("route {} -> {}", route.inbound, route.outbound);
        let projection = format!(
            "projection kind={} payload={} via={} target={}",
            input.kind.to_uppercase(),
            input.payload,
            route,
            resources.outbound_subject
        );
        if projection.len() > MAX_EVENT_BYTES {
            return Outcome::Fault("Outbound event too large".into());
        }
        Outcome::Next(projection)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== M132 NATS Reference Demo ===");

    let nats_url = std::env::var("RANVIER_ECOSYSTEM_NATS_URL")
        .unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string());
    let client = timeout(Duration::from_secs(3), async_nats::connect(nats_url))
        .await
        .map_err(|_| anyhow!("NATS connection timed out"))?
        .map_err(|_| anyhow!("NATS connection unavailable"))?;
    run_demo(client).await
}

async fn run_demo(client: async_nats::Client) -> Result<()> {
    let prefix = format!("ranvier.m132.nats.{}", client.new_inbox());
    let inbound_subject = format!("{prefix}.inbound");
    let outbound_subject = format!("{prefix}.outbound");

    let inbound_subscriber = client.subscribe(inbound_subject.clone()).await?;
    let mut outbound_observer = client.subscribe(outbound_subject.clone()).await?;

    client.flush().await?; // Confirm subscriptions before publishing demo inputs.

    let resources = AppResources {
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
        let event = maybe_event
            .ok_or_else(|| anyhow!("inbound subject closed"))?
            .map_err(|_| anyhow!("Invalid inbound NATS event"))?;

        let mut bus = Bus::new();
        match local_demo_outcome(axon.execute(event, &resources, &mut bus).await)? {
            Outcome::Next(projected) => {
                timeout(Duration::from_secs(3), sink.send_event(projected))
                    .await
                    .map_err(|_| anyhow!("Publication requires reconciliation"))?
                    .map_err(|_| anyhow!("Publication requires reconciliation"))?;
            }
            other => {
                let _ = other;
                return Err(anyhow!("Event execution requires reconciliation"));
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

    let expected: Vec<_> = ["ORDER.CREATED", "ORDER.COMPLETED"].into_iter().map(|kind| format!("projection kind={kind} payload=id=o_100 via=route {inbound_subject} -> {outbound_subject} target={outbound_subject}")).collect();
    anyhow::ensure!(
        received == expected,
        "Observed NATS projections did not match the inputs"
    );
    println!("published_to={}", inbound_subject);
    println!("received_from={}", outbound_subject);
    println!("Observed outbound events: {}", received.len());
    println!("done");

    Ok(())
}

#[cfg(test)]
mod report_tests {
    use super::*;
    use ranvier_runtime::{
        CompletionConfirmation, EffectBoundary, ExecutionReport, PersistenceOperation,
        SagaRollbackOutcome, SagaRollbackReport, SagaRollbackStep,
    };
    fn resources() -> AppResources {
        AppResources {
            inbound_subject: "fixture.in".into(),
            outbound_subject: "fixture.out".into(),
        }
    }
    #[tokio::test]
    async fn malformed_event_or_missing_route_is_a_fault() {
        let res = resources();
        for value in [
            "",
            "missing-delimiter",
            ":payload",
            "kind:",
            "bad kind:payload",
        ] {
            assert!(matches!(
                ParseEventTransition
                    .run(value.into(), &res, &mut Bus::new())
                    .await,
                Outcome::Fault(_)
            ));
        }
        let input = ParsedEvent {
            kind: "order.created".into(),
            payload: "id=fixture".into(),
        };
        assert!(matches!(
            ProjectToSinkTransition
                .run(input, &res, &mut Bus::new())
                .await,
            Outcome::Fault(_)
        ));
        let axon = Axon::<String, String, String, AppResources>::new("local")
            .then(ParseEventTransition)
            .then(ProjectToSinkTransition);
        let result = local_demo_outcome(
            axon.execute("order.created:id=fixture".into(), &res, &mut Bus::new())
                .await,
        )
        .unwrap();
        assert!(
            matches!(result, Outcome::Next(ref projected) if projected.contains("ORDER.CREATED payload=id=fixture"))
        );
    }
    #[tokio::test]
    #[ignore = "requires an explicitly configured disposable NATS broker"]
    async fn actual_broker_demo_and_invalid_utf8_are_verified() {
        let url = std::env::var("RANVIER_ECOSYSTEM_NATS_URL").expect("explicit test broker URL");
        let client = timeout(Duration::from_secs(3), async_nats::connect(url))
            .await
            .unwrap()
            .unwrap();
        run_demo(client.clone()).await.unwrap();
        let subject = client.new_inbox();
        let subscriber = client.subscribe(subject.clone()).await.unwrap();
        client.flush().await.unwrap();
        let mut source = NatsSubjectSource { subscriber };
        client
            .publish(subject.clone(), Bytes::from_static(b"\xff"))
            .await
            .unwrap();
        client
            .publish(
                subject,
                Bytes::from_static(b"order.created:id=after-invalid"),
            )
            .await
            .unwrap();
        client.flush().await.unwrap();
        let invalid = timeout(Duration::from_secs(3), source.next_event())
            .await
            .unwrap()
            .unwrap();
        assert!(invalid.is_err()); // Invalid bytes are not mistaken for source closure.
        let valid = timeout(Duration::from_secs(3), source.next_event())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(valid, "order.created:id=after-invalid");
    }

    #[test]
    fn local_report_policy_preserves_faults_and_rejects_uncertain_or_incomplete_execution() {
        let next = || Outcome::<u32, String>::Next(7);
        let reports = [
            ExecutionReport::NotRequested {
                result: next(),
                cleanup: None,
            },
            ExecutionReport::Recorded {
                result: next(),
                completion: CompletionConfirmation::Confirmed,
                cleanup: None,
            },
            ExecutionReport::Recorded {
                result: next(),
                completion: CompletionConfirmation::NotRequested,
                cleanup: None,
            },
            ExecutionReport::Unconfirmed {
                result: next(),
                operation: PersistenceOperation::Result,
                effect: EffectBoundary::MayHaveRun,
                cleanup: None,
            },
            ExecutionReport::BlockedBeforeEffect {
                operation: PersistenceOperation::Intent,
                effect: EffectBoundary::NotInvoked,
                cleanup: None,
            },
            ExecutionReport::BlockedBeforeEffect {
                operation: PersistenceOperation::Intent,
                effect: EffectBoundary::MayHaveRun,
                cleanup: None,
            },
        ];
        for report in reports {
            assert_eq!(
                local_demo_outcome(report).unwrap_err().to_string(),
                "Execution requires reconciliation before retry"
            );
        }
        for (finished, outcome) in [
            (false, SagaRollbackOutcome::Completed),
            (true, SagaRollbackOutcome::Fault),
            (true, SagaRollbackOutcome::InProgress),
        ] {
            let report = ExecutionReport::Unpersisted {
                result: next(),
                cleanup: Some(SagaRollbackReport {
                    trace_id: "private-trace".into(),
                    finished,
                    steps: vec![SagaRollbackStep {
                        node_id: "private-node".into(),
                        outcome,
                    }],
                }),
            };
            assert_eq!(
                local_demo_outcome(report).unwrap_err().to_string(),
                "Execution requires reconciliation before retry"
            );
        }
        assert!(matches!(
            local_demo_outcome(ExecutionReport::Unpersisted {
                result: next(),
                cleanup: None
            })
            .unwrap(),
            Outcome::Next(7)
        ));
        assert!(
            matches!(local_demo_outcome(ExecutionReport::Unpersisted { result: Outcome::<u32, String>::Fault("expected-domain-failure".into()), cleanup: None }).unwrap(), Outcome::Fault(error) if error == "expected-domain-failure")
        );
    }
}
