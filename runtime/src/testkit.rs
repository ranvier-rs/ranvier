use crate::Axon;
use ranvier_core::{Bus, Outcome, transition::ResourceRequirement};
use std::any::Any;

/// Lightweight helper for Axon unit tests.
///
/// Provides explicit Bus setup and resource injection so tests can mock
/// dependencies without hidden framework wiring.
pub struct AxonTestKit<R> {
    resources: R,
    bus: Bus,
}

impl<R> AxonTestKit<R> {
    pub fn new(resources: R) -> Self {
        Self {
            resources,
            bus: Bus::new(),
        }
    }

    pub fn with_bus(resources: R, bus: Bus) -> Self {
        Self { resources, bus }
    }

    pub fn insert<T: Any + Send + Sync + 'static>(&mut self, value: T) -> &mut Self {
        self.bus.insert(value);
        self
    }

    pub fn bus(&self) -> &Bus {
        &self.bus
    }

    pub fn bus_mut(&mut self) -> &mut Bus {
        &mut self.bus
    }

    pub fn resources(&self) -> &R {
        &self.resources
    }

    pub fn resources_mut(&mut self) -> &mut R {
        &mut self.resources
    }

    pub fn into_parts(self) -> (R, Bus) {
        (self.resources, self.bus)
    }
}

impl<R> AxonTestKit<R>
where
    R: ResourceRequirement,
{
    pub async fn run<In, Out, E>(
        &mut self,
        axon: &Axon<In, Out, E, R>,
        input: In,
    ) -> Outcome<Out, E>
    where
        In: Send + Sync + 'static,
        Out: Send + Sync + 'static,
        E: Send + Sync + std::fmt::Debug + 'static,
    {
        axon.execute(input, &self.resources, &mut self.bus).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ranvier_core::{Outcome, Transition};
    use std::convert::Infallible;

    #[derive(Clone)]
    struct MockResources {
        offset: usize,
    }

    impl ranvier_core::transition::ResourceRequirement for MockResources {}

    #[derive(Clone)]
    struct SumWithBus;

    #[async_trait::async_trait]
    impl Transition<usize, usize> for SumWithBus {
        type Error = Infallible;
        type Resources = MockResources;

        async fn run(
            &self,
            state: usize,
            resources: &Self::Resources,
            bus: &mut Bus,
        ) -> Outcome<usize, Self::Error> {
            let bus_value = bus.read::<usize>().copied().unwrap_or_default();
            Outcome::next(state + resources.offset + bus_value)
        }
    }

    #[tokio::test]
    async fn testkit_executes_with_mocked_resources_and_bus_values() {
        let axon = Axon::<usize, usize, Infallible, MockResources>::new("Sum").then(SumWithBus);
        let mut kit = AxonTestKit::new(MockResources { offset: 7 });
        kit.insert(5usize);

        let result = kit.run(&axon, 10).await;
        match result {
            Outcome::Next(value) => assert_eq!(value, 22),
            _ => panic!("expected Outcome::Next"),
        }
    }
}
