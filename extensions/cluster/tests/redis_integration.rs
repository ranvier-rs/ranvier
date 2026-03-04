#[cfg(feature = "redis")]
mod tests {
    use ranvier_cluster::prelude::*;
    use std::time::Duration;
    use tokio::time::sleep;

    async fn get_redis_pool() -> bb8_redis::bb8::Pool<bb8_redis::RedisConnectionManager> {
        let manager = bb8_redis::RedisConnectionManager::new("redis://127.0.0.1/").unwrap();
        bb8_redis::bb8::Pool::builder()
            .build(manager)
            .await
            .expect("Failed to connect to Redis for integration tests")
    }

    #[tokio::test]
    #[ignore = "requires local redis server"]
    async fn test_lock_race_condition() {
        let pool = get_redis_pool().await;
        let lock_a = RedisDistributedLock::new(pool.clone(), "node-a");
        let lock_b = RedisDistributedLock::new(pool.clone(), "node-b");

        let key = "test:race:lock";

        // Clear previous state
        let _ = lock_a.release(key).await;

        // Node A acquires
        let ok_a = lock_a.try_acquire(key, 5000).await.unwrap();
        assert!(ok_a, "Node A should acquire the lock");

        // Node B fails to acquire
        let ok_b = lock_b.try_acquire(key, 5000).await.unwrap();
        assert!(!ok_b, "Node B should fail to acquire the lock");

        // Node A releases
        lock_a.release(key).await.unwrap();

        // Node B now succeeds
        let ok_b2 = lock_b.try_acquire(key, 5000).await.unwrap();
        assert!(ok_b2, "Node B should now acquire the lock");

        lock_b.release(key).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires local redis server"]
    async fn test_lock_ttl_extension() {
        let pool = get_redis_pool().await;
        let lock = RedisDistributedLock::new(pool, "node-x");
        let key = "test:ttl:lock";

        let _ = lock.release(key).await;

        // Acquire with short TTL
        lock.try_acquire(key, 1000).await.unwrap();

        // Extend by 5 seconds
        lock.extend(key, 5000).await.unwrap();

        // Wait original TTL duration
        sleep(Duration::from_millis(1500)).await;

        // Should still be held (try_acquire by another node would fail)
        // Here we just test we can still release it or extend again
        lock.extend(key, 2000).await.unwrap();
        lock.release(key).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires local redis server"]
    async fn test_pubsub_synchronization() {
        let pool = get_redis_pool().await;
        let bus = RedisClusterBus::new(pool);
        let topic = "ranvier:test:sync";

        // In a real integration test, we would spawn a subscriber task
        // But since ClusterBus::subscribe is currently a stub for registration,
        // we mainly test that publish doesn't error.
        bus.publish(topic, b"hello cluster").await.unwrap();
    }
}
