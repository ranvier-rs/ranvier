use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{watch, RwLock};
use std::any::Any;

/// A dynamic wrapper around a policy value that can be updated in real-time.
///
/// Uses `tokio::sync::watch` for efficient, lock-free (mostly) reads of the latest value.
#[derive(Clone)]
pub struct DynamicPolicy<P> {
    receiver: watch::Receiver<P>,
}

impl<P: Clone + Send + Sync + 'static> DynamicPolicy<P> {
    pub fn new(initial: P) -> (watch::Sender<P>, Self) {
        let (tx, rx) = watch::channel(initial);
        (tx, Self { receiver: rx })
    }

    /// Access the current value of the policy.
    pub fn current(&self) -> P {
        self.receiver.borrow().clone()
    }

    /// Get a cloned receiver for manual watching if needed.
    pub fn receiver(&self) -> watch::Receiver<P> {
        self.receiver.clone()
    }
}

/// Global or runtime-scoped registry for managing dynamic policies.
#[derive(Debug, Default, Clone)]
pub struct PolicyRegistry {
    policies: Arc<RwLock<HashMap<String, Box<dyn Any + Send + Sync>>>>,
}

impl PolicyRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new dynamic policy.
    pub async fn register<P>(&self, name: impl Into<String>, initial: P) -> DynamicPolicy<P>
    where
        P: Clone + Send + Sync + 'static,
    {
        let name = name.into();
        let (tx, dp) = DynamicPolicy::new(initial);
        
        let mut guard = self.policies.write().await;
        guard.insert(name, Box::new(tx));
        dp
    }

    /// Update an existing policy by name.
    pub async fn update<P>(&self, name: &str, new_value: P) -> anyhow::Result<()>
    where
        P: Clone + Send + Sync + 'static,
    {
        let guard = self.policies.read().await;
        let sender_any = guard.get(name)
            .ok_or_else(|| anyhow::anyhow!("Policy '{}' not found in registry", name))?;
        
        let sender = sender_any.downcast_ref::<watch::Sender<P>>()
            .ok_or_else(|| anyhow::anyhow!("Type mismatch for policy '{}'", name))?;
        
        sender.send(new_value)?;
        Ok(())
    }

    /// Get a handle to a dynamic policy. 
    /// Note: This typically requires knowing the name and type.
    pub async fn get_sender<P>(&self, name: &str) -> Option<watch::Sender<P>>
    where
        P: Clone + Send + Sync + 'static,
    {
        let guard = self.policies.read().await;
        guard.get(name)?.downcast_ref::<watch::Sender<P>>().cloned()
    }
}
