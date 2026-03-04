use crate::job::{Job, JobId};
use chrono::Utc;
use ranvier_core::bus::Bus;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::task::JoinSet;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace};

/// Manages and executes background jobs according to their triggers.
pub struct Scheduler {
    jobs: Arc<RwLock<HashMap<JobId, Arc<dyn Job>>>>,
    shutdown_token: CancellationToken,
}

impl Scheduler {
    /// Creates a new, empty Scheduler.
    pub fn new() -> Self {
        Self {
            jobs: Arc::new(RwLock::new(HashMap::new())),
            shutdown_token: CancellationToken::new(),
        }
    }

    /// Returns a cancellation token that can be used to stop the scheduler gracefully.
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown_token.clone()
    }

    /// Registers a new job with the scheduler.
    pub async fn add_job<J: Job>(&self, job: J) {
        let id = job.id().to_string();
        let mut jobs = self.jobs.write().await;
        jobs.insert(id, Arc::new(job));
    }

    /// Removes a job from the scheduler by ID.
    pub async fn remove_job(&self, id: &str) {
        let mut jobs = self.jobs.write().await;
        jobs.remove(id);
    }

    /// Starts the scheduler loop. This method blocks the current spawned task
    /// until the shutdown token is triggered.
    pub async fn start(self) {
        info!("Ranvier Scheduler started.");

        let mut interval = tokio::time::interval(Duration::from_millis(500));
        let jobs_ref = self.jobs.clone();

        // We track the next intended execution time for each job to avoid
        // drift or double-firing within the same second.
        let mut next_execs: HashMap<JobId, chrono::DateTime<Utc>> = HashMap::new();
        let mut join_set = JoinSet::new();

        loop {
            tokio::select! {
                _ = self.shutdown_token.cancelled() => {
                    info!("Ranvier Scheduler received shutdown signal, waiting for running jobs...");
                    break;
                }
                _ = interval.tick() => {
                    let now = Utc::now();
                    let jobs = jobs_ref.read().await;

                    for (id, job) in jobs.iter() {
                        let next = next_execs.entry(id.clone()).or_insert_with(|| {
                            job.trigger().next(now).unwrap_or(now)
                        });

                        if now >= *next {
                            // Time to fire the job
                            debug!(job_id = %id, "Executing scheduled job");

                            // Spawn the job execution to avoid blocking the scheduler loop
                            let job_clone = job.clone();
                            let job_id_clone = id.clone();

                            join_set.spawn(async move {
                                // For standalone jobs, we instantiate a fresh Bus.
                                // Alternatively, a global Bus or Bus factory could be passed.
                                let mut bus = Bus::new();
                                job_clone.execute(&mut bus).await;
                                trace!(job_id = %job_id_clone, "Job execution completed");
                            });

                            // Calculate the *next* tick after this one
                            if let Some(new_next) = job.trigger().next(now) {
                                *next = new_next;
                            } else {
                                // If the trigger is exhausted (unlikely for chron/interval),
                                // we push it far into the future.
                                *next = now + chrono::Duration::days(3650);
                            }
                        }
                    }
                }
                Some(res) = join_set.join_next() => {
                    if let Err(e) = res {
                        error!("Job task panicked or was cancelled: {}", e);
                    }
                }
            }
        }

        while let Some(res) = join_set.join_next().await {
            if let Err(e) = res {
                error!("Job task panicked or was cancelled during shutdown: {}", e);
            }
        }

        info!("Ranvier Scheduler stopped.");
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}
