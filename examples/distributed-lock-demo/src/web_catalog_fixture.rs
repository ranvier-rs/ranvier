// Actual public catalog string, using the demo's real lease and task types.
#[rustfmt::skip]
#[tokio::test]
#[ignore = "requires an isolated Redis fixture"]
async fn public_lock_catalog_executes() -> anyhow::Result<()> {
    let url = std::env::var("RANVIER_LEASE_TEST_REDIS")?;
    let redis_url = url.as_str();
    let key = format!("ranvier:m426:catalog:{}", uuid::Uuid::new_v4());
    let axon = Axon::<CriticalTask, CriticalTask, String>::new("catalog-lease").then(LockedProcessor { lock_key: key });
    let task = CriticalTask { task_id: "catalog".into(), payload: "synthetic".into() };
    let mut bus = Bus::new();
bus.insert(DistributedLockHandle::new(redis_url, 5000).map_err(anyhow::Error::msg)?);
let report = axon.execute(task, &(), &mut bus).await;
// Local workflow: a Redis lease does not establish workflow durability.
if report.cleanup().is_some_and(|cleanup| !cleanup.is_complete()) {
    anyhow::bail!("Execution requires reconciliation before retry");
}
match report {
    ExecutionReport::Unpersisted { result: Outcome::Next(result), .. } => {
        println!("Worker status: {}", result.status);
    }
    _ => anyhow::bail!("Execution requires reconciliation before retry"),
}
    Ok(())
}

#[tokio::test]
#[ignore = "requires an isolated Redis fixture"]
async fn real_redis_stale_owner_cannot_release_successor() -> anyhow::Result<()> {
    let url = std::env::var("RANVIER_LEASE_TEST_REDIS")?;
    let lock = DistributedLockHandle::new(&url, 300).map_err(anyhow::Error::msg)?;
    let successor_lock = DistributedLockHandle::new(&url, 5000).map_err(anyhow::Error::msg)?;
    let key = format!("ranvier:m426:ownership:{}", uuid::Uuid::new_v4());
    let first = lock.try_acquire(&key).await.map_err(anyhow::Error::msg)?.expect("first owner");
    assert!(successor_lock.try_acquire(&key).await.map_err(anyhow::Error::msg)?.is_none());
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    let successor = successor_lock.try_acquire(&key).await.map_err(anyhow::Error::msg)?.expect("successor owner");
    assert_ne!(first.token, successor.token);
    assert!(!lock.release(&first).await.map_err(anyhow::Error::msg)?);
    assert!(successor_lock.try_acquire(&key).await.map_err(anyhow::Error::msg)?.is_none());
    assert!(successor_lock.release(&successor).await.map_err(anyhow::Error::msg)?);
    assert!(!successor_lock.release(&successor).await.map_err(anyhow::Error::msg)?);
    Ok(())
}

#[tokio::test]
async fn missing_capability_and_connection_failure_are_faults() {
    assert!(DistributedLockHandle::new("redis://127.0.0.1:1", 0).is_err());
    assert!(DistributedLockHandle::new("redis://127.0.0.1:1", 60001).is_err());
    assert_eq!(DistributedLockHandle::new("invalid-private-sentinel", 5000).err().as_deref(), Some("Invalid Redis configuration"));
    let lock = DistributedLockHandle::new("redis://127.0.0.1:1", 5000).unwrap();
    assert!(lock.try_acquire("synthetic").await.is_err());
    let processor = LockedProcessor { lock_key: "synthetic".into() };
    let task = || CriticalTask { task_id: "synthetic".into(), payload: "private-payload".into() };
    let mut bus = Bus::new();
    assert!(matches!(processor.run(task(), &(), &mut bus).await, Outcome::Fault(message) if message == "Lock capability unavailable"));
    bus.insert(lock);
    assert!(matches!(processor.run(task(), &(), &mut bus).await, Outcome::Fault(message) if message == "Lock acquisition unconfirmed; reconcile before retry"));
}
