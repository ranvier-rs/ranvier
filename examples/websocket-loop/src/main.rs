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
use ranvier_core::event::{EventSink, EventSource};
use ranvier_core::prelude::*;
use ranvier_runtime::Axon;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

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

// ============================================================================
// 1. Data Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WsMessage {
    id: u64,
    content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatEvent {
    message_id: u64,
    user_id: String,
    message: String,
}

struct MessageContext(u64);
const CLOSE_EVENT: &str = "connection.close";
const MAX_MESSAGE_BYTES: usize = 4096;
const MAX_QUEUED_RESPONSES: usize = 64;

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
    type Error = String;

    async fn send_event(&self, event: String) -> Result<(), Self::Error> {
        let mut outgoing = self
            .outgoing
            .lock()
            .map_err(|_| "Mock sink unavailable".to_owned())?;
        if outgoing.len() >= MAX_QUEUED_RESPONSES {
            return Err("Mock sink capacity reached".into());
        }
        outgoing.push(event);
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
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: WsMessage,
        _resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<ChatEvent, Self::Error> {
        if input.content.len() > MAX_MESSAGE_BYTES || input.content.trim().is_empty() {
            return Outcome::Fault("Invalid message size".into());
        }
        if bus.read::<MessageContext>().is_some() {
            return Outcome::Fault("Stale message context".into());
        }
        bus.insert(MessageContext(input.id));
        if input.content == "exit" {
            return Outcome::Emit(CLOSE_EVENT.into(), None);
        }

        Outcome::Next(ChatEvent {
            message_id: input.id,
            user_id: "user_1".to_string(),
            message: input.content.to_uppercase(),
        })
    }
}

/// Transition: Broadcast (Simulated) -> String Response
#[derive(Clone)]
struct Broadcast;

#[async_trait]
impl Transition<ChatEvent, String> for Broadcast {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: ChatEvent,
        _resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        if !bus
            .read::<MessageContext>()
            .is_some_and(|context| context.0 == input.message_id)
        {
            return Outcome::Fault("Message context missing or mismatched".into());
        }
        let response = format!("User {} said: {}", input.user_id, input.message);
        Outcome::Next(response)
    }
}

// ============================================================================
// 4. Main Loop
// ============================================================================

async fn deliver_report<S: EventSink<String>>(
    report: ranvier_runtime::ExecutionReport<Outcome<String, String>>,
    sink: &S,
) -> anyhow::Result<bool> {
    match local_demo_outcome(report)? {
        Outcome::Next(response) => {
            sink.send_event(response)
                .await
                .map_err(|_| anyhow::anyhow!("Sink delivery requires reconciliation"))?;
            Ok(false)
        }
        Outcome::Emit(event, None) if event == CLOSE_EVENT => Ok(true),
        Outcome::Fault(_) => anyhow::bail!("Message execution failed"),
        _ => anyhow::bail!("Unexpected message control outcome"),
    }
}

async fn run_loop(ws: &mut MockWebSocket) -> anyhow::Result<()> {
    let axon = Axon::<WsMessage, WsMessage, String>::new("ChatFlow")
        .then(ProcessMessage)
        .then(Broadcast);
    while let Some(msg) = ws.next_event().await {
        let mut bus = Bus::new();
        if deliver_report(axon.execute(msg, &(), &mut bus).await, ws).await? {
            break;
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Mock WebSocket Loop Demo ===");
    let mut ws = MockWebSocket::new(vec!["hello", "ranvier", "is logic", "exit"]);
    run_loop(&mut ws).await?;
    let sent = ws
        .outgoing
        .lock()
        .map_err(|_| anyhow::anyhow!("Mock sink unavailable"))?;
    println!("Total messages sent: {}", sent.len());
    Ok(())
}

#[cfg(test)]
mod report_tests {
    use super::*;
    use ranvier_runtime::{
        CompletionConfirmation, EffectBoundary, ExecutionReport, PersistenceOperation,
        SagaRollbackOutcome, SagaRollbackReport, SagaRollbackStep,
    };
    #[test]
    fn declared_demo_runs() {
        main().unwrap();
    }
    #[tokio::test]
    async fn messages_have_fresh_context_and_exit_stops_without_sink_delivery() {
        let mut ws = MockWebSocket::new(vec!["hello", "world", "exit", "after-close"]);
        run_loop(&mut ws).await.unwrap();
        assert_eq!(
            *ws.outgoing.lock().unwrap(),
            vec!["User user_1 said: HELLO", "User user_1 said: WORLD"]
        );
        assert_eq!(ws.incoming.len(), 1);
        assert!(matches!(
            Broadcast
                .run(
                    ChatEvent {
                        message_id: 1,
                        user_id: "user_1".into(),
                        message: "private".into()
                    },
                    &(),
                    &mut Bus::new()
                )
                .await,
            Outcome::Fault(_)
        ));
    }
    #[tokio::test]
    async fn sink_failure_stops_before_consuming_the_next_message() {
        let mut ws = MockWebSocket::new(vec!["first", "second"]);
        *ws.outgoing.lock().unwrap() = vec!["queued".into(); MAX_QUEUED_RESPONSES];
        assert_eq!(
            run_loop(&mut ws).await.unwrap_err().to_string(),
            "Sink delivery requires reconciliation"
        );
        assert_eq!(ws.incoming.len(), 1);
        assert_eq!(ws.outgoing.lock().unwrap().len(), MAX_QUEUED_RESPONSES);
    }
    #[tokio::test]
    async fn oversized_or_empty_input_does_not_reach_sink() {
        let large = "x".repeat(MAX_MESSAGE_BYTES + 1);
        for content in [large.as_str(), " "] {
            let mut ws = MockWebSocket::new(vec![content, "next"]);
            assert!(run_loop(&mut ws).await.is_err());
            assert!(ws.outgoing.lock().unwrap().is_empty());
            assert_eq!(ws.incoming.len(), 1);
        }
    }
    #[tokio::test]
    async fn unsafe_reports_and_faults_never_send_or_authorize_close() {
        let ws = MockWebSocket::new(vec![]);
        let reports = [
            ExecutionReport::Unconfirmed {
                result: Outcome::Next("private".into()),
                operation: PersistenceOperation::Result,
                effect: EffectBoundary::MayHaveRun,
                cleanup: None,
            },
            ExecutionReport::Unpersisted {
                result: Outcome::Next("private".into()),
                cleanup: Some(SagaRollbackReport {
                    trace_id: "private".into(),
                    finished: false,
                    steps: vec![],
                }),
            },
            ExecutionReport::Unpersisted {
                result: Outcome::Fault("private-error".into()),
                cleanup: None,
            },
        ];
        for report in reports {
            assert!(deliver_report(report, &ws).await.is_err());
        }
        assert!(ws.outgoing.lock().unwrap().is_empty());
        assert!(
            deliver_report(
                ExecutionReport::Unpersisted {
                    result: Outcome::Emit(CLOSE_EVENT.into(), None),
                    cleanup: None
                },
                &ws
            )
            .await
            .unwrap()
        );
        assert!(ws.outgoing.lock().unwrap().is_empty());
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
