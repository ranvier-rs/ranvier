use crate::store::{Session, SessionInner, SessionStore};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// An in-memory session store built on `HashMap`.
/// Useful for development and testing. Do not use in multi-instance production environments.
#[derive(Clone)]
pub struct MemoryStore {
    sessions: Arc<RwLock<HashMap<String, SessionInner>>>,
}

impl MemoryStore {
    /// Creates a new, empty in-memory session store.
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SessionStore for MemoryStore {
    async fn load(&self, session_id: &str) -> anyhow::Result<Option<Session>> {
        let store = self.sessions.read().await;
        Ok(store.get(session_id).cloned().map(Session::from_inner))
    }

    async fn save(&self, session: &Session) -> anyhow::Result<()> {
        let mut store = self.sessions.write().await;
        let mut session_inner = session.clone().into_inner().await;
        
        // Clear modified/destroyed flags before saving
        session_inner.is_destroyed = false;
        session_inner.is_modified = false;
        
        store.insert(session_inner.id.clone(), session_inner);
        Ok(())
    }

    async fn destroy(&self, session_id: &str) -> anyhow::Result<()> {
        let mut store = self.sessions.write().await;
        store.remove(session_id);
        Ok(())
    }
}
