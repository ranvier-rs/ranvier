use clap::Parser;
use std::time::{Duration, Instant};
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// URL to benchmark
    #[arg(short, long)]
    url: String,

    /// Number of concurrent requests
    #[arg(short, long, default_value_t = 100)]
    concurrency: usize,

    /// Duration of the benchmark in seconds
    #[arg(short, long, default_value_t = 10)]
    duration: u64,

    /// Optional Bearer Token
    #[arg(short, long)]
    token: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    
    println!("Benchmarking: {} for {} seconds with concurrency {}", args.url, args.duration, args.concurrency);
    
    let client = reqwest::Client::builder()
        .pool_idle_timeout(Duration::from_secs(15))
        .pool_max_idle_per_host(args.concurrency)
        .build()?;
        
    let client = Arc::new(client);
    let semaphore = Arc::new(Semaphore::new(args.concurrency));
    let end_time = Instant::now() + Duration::from_secs(args.duration);
    
    let mut total_requests: u64 = 0;
    let mut successful_requests: u64 = 0;
    let start_time = Instant::now();
    let mut set = JoinSet::new();

    loop {
        if Instant::now() >= end_time {
            break;
        }

        let permit = match semaphore.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_) => break,
        };

        let c = client.clone();
        let u = args.url.clone();
        let tok = args.token.clone();

        set.spawn(async move {
            let mut req = c.get(&u);
            if let Some(t) = tok {
                req = req.bearer_auth(t);
            }
            
            let res = req.send().await;
            
            // Release the permit
            drop(permit);
            
            res.is_ok() && res.unwrap().status().is_success()
        });

        // Periodically harvest results to avoid unbounded set growth
        while let Some(Ok(success)) = set.try_join_next() {
            total_requests += 1;
            if success { successful_requests += 1; }
        }
    }

    // Wait for the remaining requests to finish
    while let Some(res) = set.join_next().await {
        total_requests += 1;
        if let Ok(success) = res {
            if success { successful_requests += 1; }
        }
    }
    
    let elapsed = start_time.elapsed().as_secs_f64();
    let rps = total_requests as f64 / elapsed;

    println!("--- Benchmark complete ---");
    println!("Total Requests: {}", total_requests);
    println!("Successful Requests: {}", successful_requests);
    println!("Time Elapsed: {:.2} s", elapsed);
    println!("Requests/sec: {:.2}", rps);
    
    Ok(())
}
