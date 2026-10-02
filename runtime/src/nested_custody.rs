//! Private local custody. No serialization, Debug, or replay authority.
use std::sync::Arc;

pub(crate) struct RequiredOwnedCustody;
pub(crate) struct OwnedResources<Res>(pub(crate) Arc<Res>);
pub(crate) struct NestedSagaCustody<E, Res> {
    pub(crate) resources: Arc<Res>,
    pub(crate) handlers: ranvier_core::saga::SagaCompensationRegistry<E, Res>,
}
