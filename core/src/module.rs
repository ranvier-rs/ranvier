use crate::bus::Bus;
use crate::metadata::StepMetadata;
use async_trait::async_trait;
use thiserror::Error;

pub type ModuleResult = Result<(), ModuleError>;

#[derive(Error, Debug)]
pub enum ModuleError {
    #[error("Module processing terminated early")]
    Terminate,
    #[error("Internal module error: {0}")]
    Internal(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

#[async_trait]
pub trait Module: Send + Sync + 'static {
    fn metadata(&self) -> StepMetadata;
    async fn execute(&self, bus: &mut Bus) -> ModuleResult;
}
