use ranvier_core::replay::ReplayEngine;
use ranvier_core::timeline::{Timeline, TimelineEvent};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("--- Ranvier Studio Headless Replay Demo ---");

    // 1. Construct a mock Timeline (simulating OTLP Trace)
    let mut timeline = Timeline::new();
    let start_time = 1000;

    // Node A (Enter -> Exit)
    timeline.push(TimelineEvent::NodeEnter {
        node_id: "node-a".to_string(),
        node_label: "Auth".to_string(),
        timestamp: start_time,
    });
    timeline.push(TimelineEvent::NodeExit {
        node_id: "node-a".to_string(),
        outcome_type: "Next".to_string(),
        duration_ms: 15,
        timestamp: start_time + 15,
    });

    // Node B (Enter -> Exit)
    timeline.push(TimelineEvent::NodeEnter {
        node_id: "node-b".to_string(),
        node_label: "FetchProfile".to_string(),
        timestamp: start_time + 20,
    });
    timeline.push(TimelineEvent::NodeExit {
        node_id: "node-b".to_string(),
        outcome_type: "Next".to_string(),
        duration_ms: 50,
        timestamp: start_time + 70,
    });

    // 2. Initialize ReplayEngine
    let mut engine = ReplayEngine::new(timeline);

    // 3. Step throuh
    println!("Starting Replay...");
    loop {
        match engine.next_step() {
            Some(frame) => {
                match frame.event {
                    TimelineEvent::NodeEnter { node_label, timestamp, .. } => {
                        println!("[{:>5}] 🟢 Enter Node: {}", timestamp - start_time, node_label);
                    }
                    TimelineEvent::NodeExit { outcome_type, duration_ms, timestamp, .. } => {
                        println!("[{:>5}] 🔴 Exit Node : {} ({}ms) -> Outcome: {}", timestamp - start_time, "<unknown>", duration_ms, outcome_type);
                    }
                    _ => {}
                }
            }
            None => {
                println!("Replay Finished.");
                break;
            }
        }
        // Simulate playback delay
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    Ok(())
}
