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
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: T,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<T, Self::Error> {
        match self.level.as_str() {
            "error" => tracing::error!("{}: {:?}", self.message, input),
            "warn" => tracing::warn!("{}: {:?}", self.message, input),
            "debug" => tracing::debug!("{}: {:?}", self.message, input),
            _ => tracing::info!("{}: {:?}", self.message, input),
        }
        Outcome::next(input)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ErrorNode<T> {
    pub error_message: String,
    #[serde(skip)]
    pub _marker: PhantomData<T>,
}

impl<T> ErrorNode<T> {
    pub fn new(error_message: impl Into<String>) -> Self {
        Self {
            error_message: error_message.into(),
            _marker: PhantomData,
        }
    }
}

#[async_trait]
impl<T> Transition<T, T> for ErrorNode<T>
where
    T: Send + Sync + 'static,
{
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        _input: T,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<T, Self::Error> {
        Outcome::fault(self.error_message.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn log_node_passes_input_through() {
        let node = LogNode::<String>::new("test log", "info");
        let mut bus = Bus::new();
        let result = node.run("hello".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(ref v) if v == "hello"));
    }

    #[tokio::test]
    async fn error_node_always_faults() {
        let node = ErrorNode::<String>::new("something went wrong");
        let mut bus = Bus::new();
        let result = node.run("input".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Fault(ref e) if e == "something went wrong"));
    }
}
