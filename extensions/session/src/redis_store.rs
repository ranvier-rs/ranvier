use crate::store::{Session, SessionInner, SessionStore};
use async_trait::async_trait;
use redis::AsyncCommands;
use std::sync::Arc;

/// A Redis-backed session store.
/// Suitable for production multi-instance workloads.
#[derive(Clone)]
pub struct RedisStore {
    client: redis::Client,
    prefix: String,
}

impl RedisStore {
    /// Creates a new Redis session store using the provided connection string.
    pub fn new(url: &str, prefix: impl Into<String>) -> anyhow::Result<Self> {
        let client = redis::Client::open(url)?;
        Ok(Self {
            client,
            prefix: prefix.into(),
        })
    }

    fn key(&self, session_id: &str) -> String {
        format!("{}:{}", self.prefix, session_id)
    }
}

#[async_trait]
impl SessionStore for RedisStore {
    async fn load(&self, session_id: &str) -> anyhow::Result<Option<Session>> {
        let mut conn = self.client.get_async_connection().await?;
        let key = self.key(session_id);
        let data: Option<String> = conn.get(&key).await?;

        if let Some(json) = data {
            let inner: SessionInner = serde_json::from_str(&json)?;
            Ok(Some(Session::from_inner(inner)))
        } else {
            Ok(None)
        }
    }

    async fn save(&self, session: &Session) -> anyhow::Result<()> {
        let mut conn = self.client.get_async_connection().await?;
        let mut session_inner = session.clone().into_inner().await;
        let key = self.key(&session_inner.id);
        
        // Reset flags before serialization
        session_inner.is_modified = false;
        session_inner.is_destroyed = false;
        
        let json = serde_json::to_string(&session_inner)?;

        if let Some(expiry) = session_inner.expires_at {
            let now = chrono::Utc::now();
            let duration = expiry.signed_duration_since(now).num_seconds();
            if duration > 0 {
                conn.set_ex::<_, _, ()>(&key, json, duration as u64).await?;
            } else {
                // Already expired, destroy it
                conn.del::<_, ()>(&key).await?;
            }
        } else {
            conn.set::<_, _, ()>(&key, json).await?;
        }

        Ok(())
    }

    async fn destroy(&self, session_id: &str) -> anyhow::Result<()> {
        let mut conn = self.client.get_async_connection().await?;
        let key = self.key(session_id);
        conn.del::<_, ()>(&key).await?;
        Ok(())
    }
}
