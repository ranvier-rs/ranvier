pub mod axon;
pub mod persistence;
pub mod replay;

pub mod prelude {
    pub use crate::axon::{Axon, SchematicExportRequest};
    pub use crate::persistence::{
        CompletionState, CompensationAutoTrigger, CompensationContext, CompensationHandle,
        CompensationHook, CompensationIdempotencyHandle, CompensationIdempotencyStore,
        CompensationRetryPolicy, InMemoryCompensationIdempotencyStore, InMemoryPersistenceStore,
        PersistedTrace, PersistenceAutoComplete, PersistenceEnvelope, PersistenceHandle,
        PersistenceStore, PersistenceTraceId, ResumeCursor,
    };
    #[cfg(feature = "persistence-postgres")]
    pub use crate::persistence::{PostgresCompensationIdempotencyStore, PostgresPersistenceStore};
    #[cfg(feature = "persistence-redis")]
    pub use crate::persistence::{RedisCompensationIdempotencyStore, RedisPersistenceStore};
    pub use crate::replay::ReplayEngine;
}

pub use axon::{Axon, SchematicExportRequest};
pub use persistence::{
    CompletionState, CompensationAutoTrigger, CompensationContext, CompensationHandle,
    CompensationHook, CompensationIdempotencyHandle, CompensationIdempotencyStore,
    CompensationRetryPolicy, InMemoryCompensationIdempotencyStore, InMemoryPersistenceStore,
    PersistedTrace, PersistenceAutoComplete, PersistenceEnvelope, PersistenceHandle,
    PersistenceStore, PersistenceTraceId, ResumeCursor,
};
#[cfg(feature = "persistence-postgres")]
pub use persistence::{PostgresCompensationIdempotencyStore, PostgresPersistenceStore};
#[cfg(feature = "persistence-redis")]
pub use persistence::{RedisCompensationIdempotencyStore, RedisPersistenceStore};
pub use replay::ReplayEngine;
