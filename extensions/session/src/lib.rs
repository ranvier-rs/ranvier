//! Session management for Ranvier.

pub mod store;
pub mod memory;
#[cfg(feature = "redis-store")]
pub mod redis_store;
pub mod layer;

pub use store::{Session, SessionStore};
pub use memory::MemoryStore;
#[cfg(feature = "redis-store")]
pub use redis_store::RedisStore;
pub use layer::{SessionLayer, SessionService};

pub mod prelude {
    pub use crate::store::{Session, SessionStore};
    pub use crate::memory::MemoryStore;
    #[cfg(feature = "redis-store")]
    pub use crate::redis_store::RedisStore;
    pub use crate::layer::SessionLayer;
}
