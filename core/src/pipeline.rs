use crate::context::Context;
use crate::metadata::StepMetadata;
use crate::step::{Step, StepResult};
use async_trait::async_trait;
use uuid::Uuid;

pub struct Pipeline {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub steps: Vec<Box<dyn Step>>,
}

impl Pipeline {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            description: None,
            steps: Vec::new(),
        }
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    pub fn add_step<S: Step + 'static>(mut self, step: S) -> Self {
        self.steps.push(Box::new(step));
        self
    }
}

#[async_trait]
impl Step for Pipeline {
    fn metadata(&self) -> StepMetadata {
        StepMetadata {
            id: self.id,
            label: self.name.clone(),
            description: self.description.clone(),
            // Pipeline inputs/outputs are dynamic or composite.
            // For now, we leave them empty or we could merge inputs of first step and outputs of last step.
            // Let's keep it empty for MVP.
            inputs: vec![],
            outputs: vec![],
        }
    }

    async fn execute(&self, ctx: &mut Context) -> StepResult {
        for step in &self.steps {
            match step.execute(ctx).await {
                StepResult::Next => continue,
                StepResult::Terminate => return StepResult::Terminate,
                StepResult::Error(e) => return StepResult::Error(e),
            }
        }
        StepResult::Next
    }
}
