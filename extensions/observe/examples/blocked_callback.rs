//! Run only in an owned child process: deliberately uncooperative callback.
use opentelemetry::metrics::MeterProvider;
use ranvier_observe::{
    otlp::{Protocol, Providers, SampleRatio},
    redaction::{Mode, Policy},
};
use std::{
    io::{self, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let providers = Providers::build(
        "blocked-callback-fixture",
        "http://127.0.0.1:1",
        Protocol::HttpProtobuf,
        Policy::new(Mode::Strict, "")?,
        SampleRatio::new(0.0)?,
        Duration::from_millis(100),
    )?;
    let active = Arc::new(AtomicBool::new(false));
    let callback_active = active.clone();
    let _gauge = providers
        .metrics
        .meter("blocked-callback-fixture")
        .u64_observable_gauge("fixture_blocked_callback")
        .with_callback(move |_| {
            callback_active.store(true, Ordering::Release);
            println!("CALLBACK_ENTERED");
            // Explicit flush lets the parent distinguish callback entry from startup delay.
            let _ = io::stdout().flush();
            loop {
                std::thread::park();
            }
        })
        .build();
    let start = Instant::now();
    let result = providers.shutdown();
    if result != Err(vec![ranvier_observe::otlp::ShutdownSignal::Metrics])
        || !active.load(Ordering::Acquire)
    {
        return Err("blocked callback fixture did not reach expected shutdown state".into());
    }
    println!(
        "SHUTDOWN_RETURNED_METRICS_FAILURE elapsed_ms={}",
        start.elapsed().as_millis()
    );
    println!("CALLBACK_STILL_ACTIVE");
    io::stdout().flush()?;
    // Preserve the worker for observation; only the owning watchdog ends this fixture.
    loop {
        std::thread::park();
    }
}
