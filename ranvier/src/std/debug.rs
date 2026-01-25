use crate::bus::Bus;
use crate::metadata::StepMetadata;
use crate::module::{Module, ModuleResult};
use async_trait::async_trait;
use uuid::Uuid;

/// A module that logs the request.
#[derive(Clone)]
pub struct LogModule {
    msg: String,
}

impl LogModule {
    pub fn new(msg: impl Into<String>) -> Self {
        Self { msg: msg.into() }
    }
}

#[async_trait]
impl Module for LogModule {
    fn metadata(&self) -> StepMetadata {
        StepMetadata {
            id: Uuid::new_v4(),
            label: "Log".to_string(),
            description: Some(self.msg.clone()),
            inputs: vec![],
            outputs: vec![],
        }
    }

    async fn execute(&self, bus: &mut Bus) -> ModuleResult {
        println!("{} - URI: {:?}", self.msg, bus.req.uri());
        Ok(())
    }
}
