use async_trait::async_trait;
use fred::prelude::*;
use fred::types::{Expiration, SetOptions};
use ranvier_core::cluster::{ClusterBus, ClusterError, DistributedLock, DistributedStore};
use std::sync::Arc;

/// A distributed coordination client backed by Redis using the `fred` crate.
#[derive(Clone)]
pub struct RedisClient {
    client: Arc<fred::clients::RedisClient>,
}

impl RedisClient {
    pub async fn new(connection_url: &str) -> Result<Self, ClusterError> {
        let config = RedisConfig::from_url(connection_url)
            .map_err(|e| ClusterError::ConnectionError(e.to_string()))?;

        let client = fred::clients::RedisClient::new(config, None, None, None);
        client
            .init()
            .await
            .map_err(|e| ClusterError::ConnectionError(e.to_string()))?;

        Ok(Self {
            client: Arc::new(client),
        })
    }
}

#[async_trait]
impl DistributedLock for RedisClient {
    async fn try_acquire(&self, key: &str, ttl_ms: u64) -> Result<bool, ClusterError> {
        // SET key value NX PX ttl_ms
        let options = SetOptions::NX;
        let expire = Expiration::PX(ttl_ms as i64);

        let acquired: Option<String> = self
            .client
            .set(key, "locked", Some(expire), Some(options), false)
            .await
            .map_err(|e| ClusterError::Internal(e.to_string()))?;

        Ok(acquired.is_some())
    }

    async fn release(&self, key: &str) -> Result<(), ClusterError> {
        let _: () = self
            .client
            .del(key)
            .await
            .map_err(|e| ClusterError::LockReleaseFailed(e.to_string()))?;
        Ok(())
    }

    async fn extend(&self, key: &str, extra_ttl_ms: u64) -> Result<(), ClusterError> {
        let _: () = self
            .client
            .expire(key, extra_ttl_ms as i64)
            .await
            .map_err(|e| ClusterError::Internal(e.to_string()))?;
        Ok(())
    }
}

#[async_trait]
impl DistributedStore for RedisClient {
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, ClusterError> {
        let val: Option<Vec<u8>> = self
            .client
            .get(key)
            .await
            .map_err(|e| ClusterError::Internal(e.to_string()))?;
        Ok(val)
    }

    async fn put(&self, key: &str, value: &[u8], ttl_ms: Option<u64>) -> Result<(), ClusterError> {
        if let Some(ttl) = ttl_ms {
            let expire = Expiration::PX(ttl as i64);
            let _: () = self
                .client
                .set(key, value, Some(expire), None, false)
                .await
                .map_err(|e| ClusterError::Internal(e.to_string()))?;
        } else {
            let _: () = self
                .client
                .set(key, value, None, None, false)
                .await
                .map_err(|e| ClusterError::Internal(e.to_string()))?;
        }

        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<(), ClusterError> {
        let _: () = self
            .client
            .del(key)
            .await
            .map_err(|e| ClusterError::Internal(e.to_string()))?;
        Ok(())
    }
}

#[async_trait]
impl ClusterBus for RedisClient {
    async fn publish(&self, topic: &str, payload: &[u8]) -> Result<(), ClusterError> {
        let _: () = self
            .client
            .publish(topic, payload)
            .await
            .map_err(|e| ClusterError::BusError(e.to_string()))?;
        Ok(())
    }

    async fn subscribe(&self, topic: &str) -> Result<(), ClusterError> {
        let _: () = self
            .client
            .subscribe(topic)
            .await
            .map_err(|e| ClusterError::BusError(e.to_string()))?;
        Ok(())
    }
}
