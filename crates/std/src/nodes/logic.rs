use async_trait::async_trait;
use rand::Rng;
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
        let mut rng = rand::thread_rng();
        if rng.gen::<f64>() < self.probability {
            Outcome::next(input)
        } else {
            let payload = serde_json::to_value(&input).ok();
            Outcome::branch(self.jump_target.clone(), payload)
        }
    }
}
