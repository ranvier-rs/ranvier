//! Session management for Ranvier.

pub mod layer;
pub mod memory;
#[cfg(feature = "redis-store")]
pub mod redis_store;
pub mod store;

pub use layer::{SessionLayer, SessionService};
pub use memory::MemoryStore;
#[cfg(feature = "redis-store")]
pub use redis_store::RedisStore;
pub use store::{Session, SessionStore};

pub mod prelude {
    pub use crate::layer::SessionLayer;
    pub use crate::memory::MemoryStore;
    #[cfg(feature = "redis-store")]
    pub use crate::redis_store::RedisStore;
    pub use crate::store::{Session, SessionStore};
}
