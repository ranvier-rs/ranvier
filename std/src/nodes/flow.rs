use async_trait::async_trait;
use ranvier_core::{bus::Bus, outcome::Outcome, transition::Transition};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::marker::PhantomData;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DelayNode<T> {
    pub duration_ms: u64,
    #[serde(skip)]
    pub _marker: PhantomData<T>,
}

impl<T> DelayNode<T> {
    pub fn new(duration_ms: u64) -> Self {
        Self {
            duration_ms,
            _marker: PhantomData,
        }
    }
}

#[async_trait]
impl<T> Transition<T, T> for DelayNode<T>
where
    T: Send + Sync + 'static,
{
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: T,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<T, Self::Error> {
        tokio::time::sleep(Duration::from_millis(self.duration_ms)).await;
        Outcome::next(input)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct IdentityNode<T> {
    #[serde(skip)]
    pub _marker: PhantomData<T>,
}

impl<T> IdentityNode<T> {
    pub fn new() -> Self {
        Self {
            _marker: PhantomData,
        }
    }
}

impl<T> Default for IdentityNode<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl<T> Transition<T, T> for IdentityNode<T>
where
    T: Send + Sync + 'static,
{
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: T,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<T, Self::Error> {
        Outcome::next(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn delay_node_passes_after_sleep() {
        let node = DelayNode::<String>::new(10); // 10ms
        let mut bus = Bus::new();
        let start = std::time::Instant::now();
        let result = node.run("data".into(), &(), &mut bus).await;
        assert!(start.elapsed().as_millis() >= 9);
        assert!(matches!(result, Outcome::Next(ref v) if v == "data"));
    }

    #[tokio::test]
    async fn identity_node_passes_through() {
        let node = IdentityNode::<i32>::new();
        let mut bus = Bus::new();
        let result = node.run(42, &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(42)));
    }
}
