use crate::context::Context;
use crate::metadata::StepMetadata;
use async_trait::async_trait;

pub type StepError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug)]
pub enum StepResult {
    Next,
    Terminate,
    Error(StepError),
}

#[async_trait]
pub trait Step: Send + Sync {
    fn metadata(&self) -> StepMetadata;
    async fn execute(&self, ctx: &mut Context) -> StepResult;
}
