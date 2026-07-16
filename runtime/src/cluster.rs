use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::time::sleep;
use tracing::{info, warn};

use ranvier_core::cancellation::CancellationToken;

// In a real scenario, this might use ranvier_core::cluster::DistributedLock or the local one in distributed.rs
use crate::distributed::{DistributedError, DistributedLock, LockOptions};

/// Interface for components participating in cluster leader election.
#[async_trait]
pub trait LeaderElection: Send + Sync {
    /// Attempt to become the leader or renew leadership if already the leader.
    async fn try_become_leader(&self) -> Result<bool, DistributedError>;

    /// Returns true if this node is currently the elected leader.
    fn is_leader(&self) -> bool;

    /// Step down from leadership explicitly.
    async fn step_down(&self) -> Result<(), DistributedError>;
}

/// A standard implementation of leader election using a distributed lock.
pub struct LockBasedElection<L: DistributedLock> {
    lock: Arc<L>,
    node_id: String,
    resource_key: String,
    is_leader: Arc<AtomicBool>,
    ttl_ms: u64,
}

impl<L: DistributedLock> LockBasedElection<L> {
    pub fn new(lock: Arc<L>, node_id: String, resource_key: String, ttl_ms: u64) -> Self {
        Self {
            lock,
            node_id,
            resource_key,
            is_leader: Arc::new(AtomicBool::new(false)),
            ttl_ms,
        }
    }
}

#[async_trait]
impl<L: DistributedLock> LeaderElection for LockBasedElection<L> {
    async fn try_become_leader(&self) -> Result<bool, DistributedError> {
        let opts = LockOptions {
            ttl_ms: self.ttl_ms,
            retry_count: 0,
            retry_delay_ms: 0,
        };

        match self.lock.acquire(&self.resource_key, opts).await {
            Ok(_guard) => {
                if !self.is_leader.load(Ordering::Relaxed) {
                    info!("Node {} became the cluster leader", self.node_id);
                    self.is_leader.store(true, Ordering::SeqCst);
                }
                // We don't save the guard here in this simple implementation,
                // but a production version would keep the guard to extend it later.
                Ok(true)
            }
            Err(DistributedError::LockError(_)) => {
                if self.is_leader.load(Ordering::Relaxed) {
                    warn!("Node {} lost leadership!", self.node_id);
                    self.is_leader.store(false, Ordering::SeqCst);
                }
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }

    fn is_leader(&self) -> bool {
        self.is_leader.load(Ordering::Relaxed)
    }

    async fn step_down(&self) -> Result<(), DistributedError> {
        if self.is_leader.swap(false, Ordering::SeqCst) {
            info!("Node {} stepping down from leadership", self.node_id);
            // In a real implementation, we would release the guard here.
        }
        Ok(())
    }
}

/// Manages the background task that periodically attempts to renew leadership.
pub struct ClusterManager {
    // Hidden internal structures for background task handle
}

impl ClusterManager {
    /// Starts a background task that periodically polls the leader election
    pub fn start_election_loop<E: LeaderElection + 'static>(
        election: Arc<E>,
        interval: Duration,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                match election.try_become_leader().await {
                    Ok(true) => {
                        // Successfully became or renewed leader.
                    }
                    Ok(false) => {
                        // Not the leader.
                    }
                    Err(err) => {
                        warn!("Error in leader election: {}", err);
                    }
                }
                sleep(interval).await;
            }
        })
    }

    /// Start a leader-election loop owned by a cooperative cancellation token.
    ///
    /// Cancellation interrupts an in-flight election attempt or interval wait,
    /// requests an explicit step-down, and completes the returned task.
    pub fn start_election_loop_with_cancellation<E: LeaderElection + 'static>(
        election: Arc<E>,
        interval: Duration,
        token: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                let election_result = tokio::select! {
                    biased;
                    _ = token.cancelled() => break,
                    result = election.try_become_leader() => result,
                };
                match election_result {
                    Ok(true) => {}
                    Ok(false) => {}
                    Err(error) => warn!("Error in leader election: {}", error),
                }

                tokio::select! {
                    biased;
                    _ = token.cancelled() => break,
                    _ = sleep(interval) => {}
                }
            }

            if let Err(error) = election.step_down().await {
                warn!(
                    "Failed to step down during election-loop cancellation: {}",
                    error
                );
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ranvier_core::cancellation::CancellationReason;
    use std::sync::atomic::AtomicUsize;

    struct RecordingElection {
        attempts: AtomicUsize,
        step_downs: AtomicUsize,
    }

    #[async_trait]
    impl LeaderElection for RecordingElection {
        async fn try_become_leader(&self) -> Result<bool, DistributedError> {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            Ok(true)
        }

        fn is_leader(&self) -> bool {
            self.attempts.load(Ordering::SeqCst) > 0
        }

        async fn step_down(&self) -> Result<(), DistributedError> {
            self.step_downs.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn cancellation_stops_election_loop_and_steps_down() {
        let election = Arc::new(RecordingElection {
            attempts: AtomicUsize::new(0),
            step_downs: AtomicUsize::new(0),
        });
        let token = CancellationToken::new();
        let handle = ClusterManager::start_election_loop_with_cancellation(
            election.clone(),
            Duration::from_secs(60),
            token.clone(),
        );

        while election.attempts.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        assert!(token.cancel(CancellationReason::OperatorShutdown));
        handle.await.expect("managed election task");

        assert_eq!(election.step_downs.load(Ordering::SeqCst), 1);
    }
}
