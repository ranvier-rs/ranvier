use crate::step::Step;
use tower::{Layer, ServiceBuilder};

/// A builder for creating Ranvier pipelines.
///
/// This is a wrapper around `tower::ServiceBuilder` that enforces `Step` usage
/// and likely adds Ranvier-specific context management in the future.
#[derive(Debug, Clone)]
pub struct Pipeline<L> {
    builder: ServiceBuilder<L>,
}

impl Pipeline<tower::layer::util::Identity> {
    /// Create a new empty pipeline.
    pub fn new() -> Self {
        Self {
            builder: ServiceBuilder::new(),
        }
    }
}

impl<L> Pipeline<L> {
    /// Add a generic Tower Layer to the pipeline.
    pub fn layer<T>(self, layer: T) -> Pipeline<tower::layer::util::Stack<T, L>> {
        Pipeline {
            builder: self.builder.layer(layer),
        }
    }

    /// Add a Ranvier Step to the pipeline.
    ///
    /// This is semantically same as `layer`, but restricts T to be a Step.
    /// In the future, this might wrap the step to record execution status in Context.
    pub fn step<S, Service>(self, step: S) -> Pipeline<tower::layer::util::Stack<S, L>>
    where
        S: Step<Service>,
        L: Layer<Service>,
    {
        self.layer(step)
    }

    /// Consume the pipeline builder and wrap a service.
    ///
    /// Returns the final Service that processes requests.
    pub fn service<S>(self, service: S) -> L::Service
    where
        L: Layer<S>,
    {
        self.builder.service(service)
    }
}
