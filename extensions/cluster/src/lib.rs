// ranvier-cluster

#[cfg(feature = "redis")]
pub mod redis_impl;

pub mod prelude {
    pub use ranvier_core::cluster::{ClusterBus, ClusterError, DistributedLock};
    #[cfg(feature = "redis")]
    pub use crate::redis_impl::{RedisClusterBus, RedisDistributedLock};
}
