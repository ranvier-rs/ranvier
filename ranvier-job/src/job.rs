use crate::Trigger;
use async_trait::async_trait;
use ranvier_core::bus::Bus;
use ranvier_core::outcome::Outcome;
use ranvier_core::transition::ResourceRequirement;
use ranvier_runtime::Axon;

/// Unique identifier for a registered background job.
pub type JobId = String;

/// The trait representing an executable job.
#[async_trait]
pub trait Job: Send + Sync + 'static {
    /// The unique identifier of this job.
    fn id(&self) -> &str;

    /// The trigger schedule for this job.
    fn trigger(&self) -> &Trigger;

    /// Execute the inner background task.
    ///
    /// The `bus` provided here is managed by the scheduler and contains
    /// resources requested or shared globally.
    async fn execute(&self, bus: &mut Bus);
}

/// A wrapper to easily register `Axon` instances as scheduled background jobs.
pub struct AxonJob<In, Out, E, Res> {
    id: JobId,
    trigger: Trigger,
    axon: Axon<In, Out, E, Res>,
    input: In,
    resources: Res,
}

impl<In, Out, E, Res> AxonJob<In, Out, E, Res> {
    pub fn new(id: impl Into<String>, trigger: Trigger, axon: Axon<In, Out, E, Res>, input: In, resources: Res) -> Self {
        Self {
            id: id.into(),
            trigger,
            axon,
            input,
            resources,
        }
    }
}

#[async_trait]
impl<In, Out, E, Res> Job for AxonJob<In, Out, E, Res>
where
    In: Send + Sync + Clone + serde::Serialize + serde::de::DeserializeOwned + 'static,
    Out: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
    E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    Res: ResourceRequirement + Send + Sync + 'static,
{
    fn id(&self) -> &str {
        &self.id
    }

    fn trigger(&self) -> &Trigger {
        &self.trigger
    }

    async fn execute(&self, bus: &mut Bus) {
        // We clone the input because we need to run it repeatedly
        let state = self.input.clone();
        // Discard the output/error; in a background job there's no immediate return to a caller
        let _ = self.axon.execute(state, &self.resources, bus).await;
    }
}

/// A simple closure-based Job implementation.
pub struct ClosureJob<F> {
    id: JobId,
    trigger: Trigger,
    func: F,
}

impl<F> ClosureJob<F>
where
    F: for<'a> Fn(&'a mut Bus) -> futures::future::BoxFuture<'a, ()> + Send + Sync + 'static,
{
    pub fn new(id: impl Into<String>, trigger: Trigger, func: F) -> Self {
        Self {
            id: id.into(),
            trigger,
            func,
        }
    }
}

#[async_trait]
impl<F> Job for ClosureJob<F>
where
    F: for<'a> Fn(&'a mut Bus) -> futures::future::BoxFuture<'a, ()> + Send + Sync + 'static,
{
    fn id(&self) -> &str {
        &self.id
    }

    fn trigger(&self) -> &Trigger {
        &self.trigger
    }

    async fn execute(&self, bus: &mut Bus) {
        (self.func)(bus).await;
    }
}
