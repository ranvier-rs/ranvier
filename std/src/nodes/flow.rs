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
    type Error = std::convert::Infallible;

    async fn run(&self, input: T, _bus: &mut Bus) -> Outcome<T, Self::Error> {
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
    type Error = std::convert::Infallible;

    async fn run(&self, input: T, _bus: &mut Bus) -> Outcome<T, Self::Error> {
        Outcome::next(input)
    }
}
