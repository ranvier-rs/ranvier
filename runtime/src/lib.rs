#![allow(deprecated)]

pub mod axon;
pub mod cluster;
pub mod distributed;
pub mod llm;
pub mod persistence;
pub mod replay;
pub mod retry;
pub mod testkit;

pub mod prelude {
    pub use crate::axon::{Axon, BoxFuture, ExecutionMode, ParallelStrategy, SchematicExportRequest};
    pub use crate::{InfallibleAxon, SimpleAxon, TypedAxon};
    pub use crate::cluster::{ClusterManager, LeaderElection, LockBasedElection};
    pub use crate::distributed::{
        DistributedError, DistributedLock, DistributedStore, Guard, LockOptions,
    };
    pub use crate::llm::{LlmError, LlmProvider, LlmTemplateVars, LlmTransition, MockLlmConfig};
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
    pub use crate::retry::{BackoffStrategy, RetryPolicy};
    pub use crate::testkit::AxonTestKit;
}

/// Axon with `String` error — the most common pattern for examples and prototyping.
pub type SimpleAxon<In, Out, Res = ()> = Axon<In, Out, String, Res>;

/// Axon with `RanvierError` — typed error categories without a custom enum.
pub type TypedAxon<In, Out, Res = ()> = Axon<In, Out, ranvier_core::error::RanvierError, Res>;

/// Axon with `Never` error — for pipelines that cannot fail.
///
/// Uses `ranvier_core::Never` instead of `std::convert::Infallible`
/// because `Never` satisfies the `Serialize + DeserializeOwned` bounds
/// required by `Axon`.
pub type InfallibleAxon<In, Out, Res = ()> = Axon<In, Out, ranvier_core::Never, Res>;

pub use axon::{Axon, ParallelStrategy, SchematicExportRequest};
pub use cluster::{ClusterManager, LeaderElection, LockBasedElection};
pub use distributed::{DistributedError, DistributedLock, DistributedStore, Guard, LockOptions};
pub use llm::{LlmError, LlmProvider, LlmTemplateVars, LlmTransition, MockLlmConfig};
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
pub use retry::{BackoffStrategy, RetryPolicy};
pub use testkit::AxonTestKit;
