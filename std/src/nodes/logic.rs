use async_trait::async_trait;
use ranvier_core::{bus::Bus, outcome::Outcome, transition::Transition};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::marker::PhantomData;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RandomBranchNode<T> {
    pub probability: f64,
    pub jump_target: String,
    #[serde(skip)]
    pub _marker: PhantomData<T>,
}

impl<T> RandomBranchNode<T> {
    pub fn new(probability: f64, jump_target: impl Into<String>) -> Self {
        Self {
            probability,
            jump_target: jump_target.into(),
            _marker: PhantomData,
        }
    }
}

#[async_trait]
impl<T> Transition<T, T> for RandomBranchNode<T>
where
    T: Send + Sync + 'static + Clone + Serialize,
{
    type Error = std::convert::Infallible;

    async fn run(&self, input: T, _bus: &mut Bus) -> Outcome<T, Self::Error> {
        if rand::random::<f64>() < self.probability {
            Outcome::next(input)
        } else {
            let payload = serde_json::to_value(&input).ok();
            Outcome::branch(self.jump_target.clone(), payload)
        }
    }
}

use std::sync::Arc;

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct FilterNode<T, F> {
    #[serde(skip)]
    pub predicate: Arc<F>,
    #[serde(skip)]
    pub _marker: PhantomData<T>,
}

impl<T, F> FilterNode<T, F>
where
    F: Fn(&T) -> bool + Send + Sync + 'static,
{
    pub fn new(predicate: F) -> Self {
        Self {
            predicate: Arc::new(predicate),
            _marker: PhantomData,
        }
    }
}

// Clone is now always derived-able if we manually impl or just rely on Arc
impl<T, F> Clone for FilterNode<T, F> {
    fn clone(&self) -> Self {
        Self {
            predicate: self.predicate.clone(),
            _marker: PhantomData,
        }
    }
}

impl<T, F> std::fmt::Debug for FilterNode<T, F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilterNode").finish()
    }
}

#[async_trait]
impl<T, F> Transition<T, T> for FilterNode<T, F>
where
    T: Send + Sync + 'static + Serialize,
    F: Fn(&T) -> bool + Send + Sync + 'static,
{
    type Error = std::convert::Infallible;

    async fn run(&self, input: T, _bus: &mut Bus) -> Outcome<T, Self::Error> {
        if (self.predicate)(&input) {
            Outcome::next(input)
        } else {
            let payload = serde_json::to_value(&input).ok();
            Outcome::branch("rejected".to_string(), payload)
        }
    }
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct SwitchNode<T, F> {
    #[serde(skip)]
    pub matcher: Arc<F>,
    #[serde(skip)]
    pub _marker: PhantomData<T>,
}

impl<T, F> SwitchNode<T, F>
where
    F: Fn(&T) -> String + Send + Sync + 'static,
{
    pub fn new(matcher: F) -> Self {
        Self {
            matcher: Arc::new(matcher),
            _marker: PhantomData,
        }
    }
}

impl<T, F> Clone for SwitchNode<T, F> {
    fn clone(&self) -> Self {
        Self {
            matcher: self.matcher.clone(),
            _marker: PhantomData,
        }
    }
}

impl<T, F> std::fmt::Debug for SwitchNode<T, F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SwitchNode").finish()
    }
}

#[async_trait]
impl<T, F> Transition<T, T> for SwitchNode<T, F>
where
    T: Send + Sync + 'static + Serialize,
    F: Fn(&T) -> String + Send + Sync + 'static,
{
    type Error = std::convert::Infallible;

    async fn run(&self, input: T, _bus: &mut Bus) -> Outcome<T, Self::Error> {
        let branch_id = (self.matcher)(&input);
        let payload = serde_json::to_value(&input).ok();
        Outcome::branch(branch_id, payload)
    }
}
