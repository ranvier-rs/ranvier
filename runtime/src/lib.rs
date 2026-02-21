pub mod axon;
pub mod persistence;
pub mod replay;
pub mod testkit;

pub mod prelude {
    pub use crate::axon::{Axon, SchematicExportRequest};
    pub use crate::persistence::{
        CompensationAutoTrigger, CompensationContext, CompensationHandle, CompensationHook,
        CompensationIdempotencyHandle, CompensationIdempotencyStore, CompensationRetryPolicy,
        CompletionState, InMemoryCompensationIdempotencyStore, InMemoryPersistenceStore,
        PersistedTrace, PersistenceAutoComplete, PersistenceEnvelope, PersistenceHandle,
        PersistenceStore, PersistenceTraceId, ResumeCursor,
    };
    #[cfg(feature = "persistence-postgres")]
    pub use crate::persistence::{PostgresCompensationIdempotencyStore, PostgresPersistenceStore};
    #[cfg(feature = "persistence-redis")]
    pub use crate::persistence::{RedisCompensationIdempotencyStore, RedisPersistenceStore};
    pub use crate::replay::ReplayEngine;
    pub use crate::testkit::AxonTestKit;
}

pub use axon::{Axon, SchematicExportRequest};
pub use persistence::{
    CompensationAutoTrigger, CompensationContext, CompensationHandle, CompensationHook,
    CompensationIdempotencyHandle, CompensationIdempotencyStore, CompensationRetryPolicy,
    CompletionState, InMemoryCompensationIdempotencyStore, InMemoryPersistenceStore,
    PersistedTrace, PersistenceAutoComplete, PersistenceEnvelope, PersistenceHandle,
    PersistenceStore, PersistenceTraceId, ResumeCursor,
};
#[cfg(feature = "persistence-postgres")]
pub use persistence::{PostgresCompensationIdempotencyStore, PostgresPersistenceStore};
#[cfg(feature = "persistence-redis")]
pub use persistence::{RedisCompensationIdempotencyStore, RedisPersistenceStore};
pub use replay::ReplayEngine;
pub use testkit::AxonTestKit;
