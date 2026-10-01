// Actual catalog callback, interval accelerated from 60 to 1 second.
#[derive(Clone)]
struct CatalogJob { calls: Arc<AtomicU64>, active: Arc<AtomicU64>, peak: Arc<AtomicU64>, fail: bool }
#[async_trait]
impl Transition<JobContext, JobContext> for CatalogJob {
    type Error = String;
    type Resources = ();
    async fn run(&self, input: JobContext, _: &(), bus: &mut Bus) -> Outcome<JobContext, String> {
        assert!(bus.read::<u32>().is_none(), "Bus leaked between ticks");
        bus.insert(7_u32);
        self.calls.fetch_add(1, Ordering::SeqCst);
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(active, Ordering::SeqCst);
        if !self.fail { tokio::time::sleep(std::time::Duration::from_millis(1500)).await; }
        self.active.fetch_sub(1, Ordering::SeqCst);
        if self.fail { Outcome::Fault("private-job-sentinel".into()) } else { Outcome::Next(input) }
    }
}
#[rustfmt::skip]
#[tokio::test]
async fn public_scheduler_repeats_success_and_stops_fault() -> anyhow::Result<()> {
    use std::time::Duration;
    for fail in [false, true] {
        let calls = Arc::new(AtomicU64::new(0));
        let active = Arc::new(AtomicU64::new(0));
        let peak = Arc::new(AtomicU64::new(0));
        let axon = Axon::<JobContext, JobContext, String>::new("catalog-job").then(CatalogJob { calls: calls.clone(), active: active.clone(), peak: peak.clone(), fail });
// Local overlap guard; restart only after reconciling any failed run.
let halted = Arc::new(tokio::sync::Mutex::new(false));
let job = Job::new_repeated_async(
    Duration::from_secs(1),
    move |_uuid, _lock| {
        let axon = axon.clone();
        let halted = halted.clone();
        Box::pin(async move {
            let Ok(mut stopped) = halted.try_lock() else { return };
            if *stopped { return; }
            *stopped = true; // Panic/cancellation also stops future ticks.
            let ctx = JobContext { job_name: "cleanup-task".into(), run_number: 0 };
            let mut bus = Bus::new();
            let report = axon.execute(ctx, &(), &mut bus).await;
            // Intentionally local, without workflow persistence.
            let complete = !report.cleanup().is_some_and(|cleanup| !cleanup.is_complete());
            if complete && matches!(report, ExecutionReport::Unpersisted { result: Outcome::Next(_), .. }) {
                *stopped = false;
            } else {
                eprintln!("Stopped; reconcile before restarting");
            }
        })
    }
)?;
        let mut scheduler = JobScheduler::new().await?;
        scheduler.add(job).await?;
        scheduler.start().await?;
        tokio::time::sleep(Duration::from_secs(6)).await;
        scheduler.shutdown().await?;
        // Scheduler shutdown is not an active-callback drain guarantee.
        tokio::time::sleep(Duration::from_secs(2)).await;
        let count = calls.load(Ordering::SeqCst);
        if fail { assert_eq!(count, 1); } else { assert!(count >= 2, "scheduler did not repeat"); }
        assert_eq!(peak.load(Ordering::SeqCst), 1, "overlapping effect invocations");
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }
    Ok(())
}
