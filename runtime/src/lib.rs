#![allow(deprecated)]

pub mod axon;
pub mod closure_transition;
pub mod cluster;
pub mod distributed;
mod execution_observation;
pub mod execution_report;
pub mod llm;
mod nested_custody;
pub mod persistence;
pub mod replay;
pub mod retry;
pub mod saga_report;
#[cfg(feature = "streaming")]
pub mod streaming_axon;
#[cfg(feature = "streaming")]
pub mod streaming_initialization;
#[cfg(feature = "streaming")]
mod streaming_recovery;
#[cfg(feature = "streaming")]
mod streaming_recovery_admission;
#[cfg(feature = "streaming")]
pub mod streaming_report_handle;
#[cfg(feature = "streaming")]
pub mod streaming_supervisor;
pub mod testkit;

pub mod prelude {
    pub use crate::axon::{
        Axon, BoxFuture, ExecutionMode, ExecutionTerminal, ParallelBusPolicy, ParallelStrategy,
        SchematicExportRequest,
    };
    pub use crate::cluster::{ClusterManager, LeaderElection, LockBasedElection};
    pub use crate::distributed::{
        DistributedError, DistributedLock, DistributedStore, Guard, LockOptions,
    };
    pub use crate::execution_report::{
        CompletionConfirmation, EffectBoundary, ExecutionReport, PersistenceOperation,
    };
    pub use crate::llm::{LlmError, LlmProvider, LlmTemplateVars, LlmTransition, MockLlmConfig};
    pub use crate::persistence::{
        CompensationAutoTrigger, CompensationContext, CompensationHandle, CompensationHook,
        CompensationIdempotencyHandle, CompensationIdempotencyStore, CompensationRetryPolicy,
        CompletionState, InMemoryCompensationIdempotencyStore, InMemoryPersistenceStore,
        PERSISTENCE_FORMAT_VERSION, PersistedTrace, PersistenceAutoComplete, PersistenceEnvelope,
        PersistenceEventType, PersistenceHandle, PersistenceStore, PersistenceTraceId,
        ResumeCursor,
    };
    #[cfg(feature = "persistence-postgres")]
    pub use crate::persistence::{PostgresCompensationIdempotencyStore, PostgresPersistenceStore};
    #[cfg(feature = "persistence-redis")]
    pub use crate::persistence::{RedisCompensationIdempotencyStore, RedisPersistenceStore};
    pub use crate::replay::ReplayEngine;
    pub use crate::retry::{BackoffStrategy, RetryPolicy};
    pub use crate::saga_report::{
        SagaRollbackJournal, SagaRollbackOutcome, SagaRollbackReport, SagaRollbackStep,
    };
    #[cfg(feature = "streaming")]
    pub use crate::streaming_axon::{CollectedStreamingAxon, StreamTimeoutKind, StreamingAxon};
    #[cfg(feature = "streaming")]
    pub use crate::streaming_initialization::{
        StreamInitializationError, StreamInitializationReport,
    };
    pub use crate::testkit::AxonTestKit;
    pub use crate::{InfallibleAxon, SimpleAxon, TypedAxon};
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

pub use axon::{
    Axon, ExecutionTerminal, ParallelBusPolicy, ParallelStrategy, SchematicExportRequest,
};
pub use closure_transition::ClosureTransition;
pub use cluster::{ClusterManager, LeaderElection, LockBasedElection};
pub use distributed::{DistributedError, DistributedLock, DistributedStore, Guard, LockOptions};
pub use execution_report::{
    CompletionConfirmation, EffectBoundary, ExecutionReport, PersistenceOperation,
};
pub use llm::{LlmError, LlmProvider, LlmTemplateVars, LlmTransition, MockLlmConfig};
pub use persistence::{
    CompensationAutoTrigger, CompensationContext, CompensationHandle, CompensationHook,
    CompensationIdempotencyHandle, CompensationIdempotencyStore, CompensationRetryPolicy,
    CompletionState, InMemoryCompensationIdempotencyStore, InMemoryPersistenceStore,
    PERSISTENCE_FORMAT_VERSION, PersistedTrace, PersistenceAutoComplete, PersistenceEnvelope,
    PersistenceEventType, PersistenceHandle, PersistenceStore, PersistenceTraceId, ResumeCursor,
};
#[cfg(feature = "persistence-postgres")]
pub use persistence::{PostgresCompensationIdempotencyStore, PostgresPersistenceStore};
#[cfg(feature = "persistence-redis")]
pub use persistence::{RedisCompensationIdempotencyStore, RedisPersistenceStore};
pub use replay::ReplayEngine;
pub use retry::{BackoffStrategy, RetryPolicy};
pub use saga_report::{
    SagaRollbackJournal, SagaRollbackOutcome, SagaRollbackReport, SagaRollbackStep,
};
#[cfg(feature = "streaming")]
pub use streaming_axon::{CollectedStreamingAxon, StreamTimeoutKind, StreamingAxon};
#[cfg(feature = "streaming")]
pub use streaming_initialization::{StreamInitializationError, StreamInitializationReport};
pub use testkit::AxonTestKit;

#[cfg(feature = "streaming")]
pub mod streaming_consumption;
#[cfg(feature = "streaming")]
pub use streaming_consumption::{
    ObservedStream, StreamConsumptionReport, StreamConsumptionTerminal,
};

#[cfg(feature = "streaming")]
mod managed_stream_task;
