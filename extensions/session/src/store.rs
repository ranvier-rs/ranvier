use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::collections::HashMap;
use uuid::Uuid;

use std::sync::Arc;
use tokio::sync::RwLock;

/// The inner mutable state of a session.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct SessionInner {
    pub id: String,
    data: HashMap<String, Value>,
    expires_at: Option<DateTime<Utc>>,
    pub is_modified: bool,
    pub is_destroyed: bool,
}

/// A session object containing arbitrary key-value data.
/// Wraps the inner state in an Arc<RwLock> for interior mutability.
#[derive(Debug, Clone)]
pub struct Session(Arc<RwLock<SessionInner>>);

impl Session {
    /// Creates a new, empty session with a generated UUID.
    pub fn new() -> Self {
        Self(Arc::new(RwLock::new(SessionInner {
            id: Uuid::new_v4().to_string(),
            data: HashMap::new(),
            expires_at: None,
            is_modified: false,
            is_destroyed: false,
        })))
    }

    /// Gets the session ID.
    pub async fn id(&self) -> String {
        self.0.read().await.id.clone()
    }

    /// Inserts a typed value into the session state.
    pub async fn insert<T: Serialize>(&self, key: &str, value: T) -> Result<(), serde_json::Error> {
        let v = serde_json::to_value(value)?;
        let mut inner = self.0.write().await;
        inner.data.insert(key.to_string(), v);
        inner.is_modified = true;
        Ok(())
    }

    /// Retrieves a typed value from the session state.
    pub async fn get<T: DeserializeOwned>(
        &self,
        key: &str,
    ) -> Option<Result<T, serde_json::Error>> {
        let inner = self.0.read().await;
        inner
            .data
            .get(key)
            .map(|v| serde_json::from_value(v.clone()))
    }

    /// Removes a value from the session state.
    pub async fn remove(&self, key: &str) {
        let mut inner = self.0.write().await;
        if inner.data.remove(key).is_some() {
            inner.is_modified = true;
        }
    }

    /// Clears all data from the session.
    pub async fn clear(&self) {
        let mut inner = self.0.write().await;
        if !inner.data.is_empty() {
            inner.data.clear();
            inner.is_modified = true;
        }
    }

    /// Destroys the session. It will be removed from the store upon saving.
    pub async fn destroy(&self) {
        let mut inner = self.0.write().await;
        inner.is_destroyed = true;
        inner.is_modified = true;
    }

    /// Checks if the session has been modified since it was loaded.
    pub async fn is_modified(&self) -> bool {
        self.0.read().await.is_modified
    }

    /// Checks if the session should be destroyed.
    pub async fn is_destroyed(&self) -> bool {
        self.0.read().await.is_destroyed
    }

    /// Sets the expiration time for this session.
    pub async fn set_expiry(&self, expiry: DateTime<Utc>) {
        let mut inner = self.0.write().await;
        inner.expires_at = Some(expiry);
        inner.is_modified = true;
    }

    /// Gets the expiration time for this session.
    pub async fn expiry(&self) -> Option<DateTime<Utc>> {
        self.0.read().await.expires_at
    }

    /// Extracts the serialized inner state (useful for stores).
    pub async fn into_inner(self) -> SessionInner {
        self.0.read().await.clone()
    }

    /// Creates a session from a loaded Inner state.
    pub fn from_inner(inner: SessionInner) -> Self {
        Self(Arc::new(RwLock::new(inner)))
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

/// Abstract trait for a session storage backend.
#[async_trait]
pub trait SessionStore: Send + Sync + 'static {
    /// Loads a session by ID.
    async fn load(&self, session_id: &str) -> anyhow::Result<Option<Session>>;

    /// Saves a session to the store.
    async fn save(&self, session: &Session) -> anyhow::Result<()>;

    /// Destroys a session in the store.
    async fn destroy(&self, session_id: &str) -> anyhow::Result<()>;
}
