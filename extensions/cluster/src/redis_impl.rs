use std::time::Duration;
use async_trait::async_trait;
use bb8_redis::{bb8::Pool, RedisConnectionManager};
use redis::{AsyncCommands, RedisError};
use ranvier_core::cluster::{ClusterBus, ClusterError, DistributedLock};
use tracing::{debug, error};

type RedisPool = Pool<RedisConnectionManager>;

/// Redis-backed implementation of DistributedLock using SET NX PX
pub struct RedisDistributedLock {
    pool: RedisPool,
    node_id: String,
}

impl RedisDistributedLock {
    /// Create a new RedisDistributedLock
    pub fn new(pool: RedisPool, node_id: impl Into<String>) -> Self {
        Self {
            pool,
            node_id: node_id.into(),
        }
    }
}

#[async_trait]
impl DistributedLock for RedisDistributedLock {
    async fn try_acquire(&self, key: &str, ttl_ms: u64) -> Result<bool, ClusterError> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| ClusterError::ConnectionError(e.to_string()))?;

        // SET key node_id NX PX ttl_ms
        let result: Option<String> = redis::cmd("SET")
            .arg(key)
            .arg(&self.node_id)
            .arg("NX")
            .arg("PX")
            .arg(ttl_ms)
            .query_async(&mut *conn)
            .await.map_err(|e| ClusterError::Internal(e.to_string()))?;

        Ok(result.is_some())
    }

    async fn release(&self, key: &str) -> Result<(), ClusterError> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| ClusterError::ConnectionError(e.to_string()))?;

        // Use Lua script to only delete if the value matches node_id lock owner
        let script = redis::Script::new(
            r#"
            if redis.call("get",KEYS[1]) == ARGV[1]
            then
                return redis.call("del",KEYS[1])
            else
                return 0
            end
            "#,
        );

        let result: i32 = script
            .key(key)
            .arg(&self.node_id)
            .invoke_async(&mut *conn)
            .await.map_err(|e| ClusterError::Internal(e.to_string()))?;

        if result == 0 {
            // Either the lock expired or we don't own it
            debug!("Failed to release lock '{}', not owned by node '{}'", key, self.node_id);
        }

        Ok(())
    }

    async fn extend(&self, key: &str, extra_ttl_ms: u64) -> Result<(), ClusterError> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| ClusterError::ConnectionError(e.to_string()))?;

        let script = redis::Script::new(
            r#"
            if redis.call("get",KEYS[1]) == ARGV[1]
            then
                return redis.call("pexpire",KEYS[1],ARGV[2])
            else
                return 0
            end
            "#,
        );

        let result: i32 = script
            .key(key)
            .arg(&self.node_id)
            .arg(extra_ttl_ms)
            .invoke_async(&mut *conn)
            .await.map_err(|e| ClusterError::Internal(e.to_string()))?;

        if result == 0 {
            Err(ClusterError::Internal("Lock extend failed or lock not owned".to_string()))
        } else {
            Ok(())
        }
    }
}

/// Redis-backed implementation of ClusterBus using Redis Pub/Sub
pub struct RedisClusterBus {
    pool: RedisPool,
}

impl RedisClusterBus {
    pub fn new(pool: RedisPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ClusterBus for RedisClusterBus {
    async fn publish(&self, topic: &str, payload: &[u8]) -> Result<(), ClusterError> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| ClusterError::ConnectionError(e.to_string()))?;

        let _: () = conn.publish(topic, payload).await.map_err(|e| ClusterError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn subscribe(&self, topic: &str) -> Result<(), ClusterError> {
        // Detailed subscription loop implementation is usually context-dependent
        // This registers the trait definition. Actual message draining operates in a separate task.
        debug!("RedisClusterBus: Subscribed intent to topic '{}'", topic);
        Ok(())
    }
}
