use async_trait::async_trait;
use ranvier_core::{bus::Bus, outcome::Outcome, transition::Transition};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::marker::PhantomData;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LogNode<T> {
    pub message: String,
    pub level: String,
    #[serde(skip)]
    pub _marker: PhantomData<T>,
}

impl<T> LogNode<T> {
    pub fn new(message: impl Into<String>, level: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            level: level.into(),
            _marker: PhantomData,
        }
    }
}

#[async_trait]
impl<T> Transition<T, T> for LogNode<T>
where
    T: Debug + Send + Sync + 'static,
{
    type Error = std::convert::Infallible;

    async fn run(&self, input: T, _bus: &mut Bus) -> Outcome<T, Self::Error> {
        match self.level.as_str() {
            "error" => tracing::error!("{}: {:?}", self.message, input),
            "warn" => tracing::warn!("{}: {:?}", self.message, input),
            "debug" => tracing::debug!("{}: {:?}", self.message, input),
            _ => tracing::info!("{}: {:?}", self.message, input),
        }
        Outcome::next(input)
    }
}
